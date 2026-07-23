use anyhow::Result;
use eframe::egui;
use ohmycopy::clients::{ClientSource, ClientsFile};
use ohmycopy::clipboard::{
    png_to_rgba, rgba_to_png, spawn_clipboard_watcher, ClipContent, ClipboardService,
};
use ohmycopy::config::Config;
use ohmycopy::engine::{EngineCore, SharedEngine};
use ohmycopy::history::HistoryStore;
use ohmycopy::inbox::{self, MIME_DIR_ZIP, MIME_MULTI_PATHS_ZIP};
use ohmycopy::net::discover::DiscoveryService;
use ohmycopy::net::tcp::{NetEvent, NetworkHub};
use ohmycopy::protocol::ContentKind;
use ohmycopy::tray::{self, AppTray, TrayAction};
use ohmycopy::ui::{NearbyDevice, OhMyCopyApp, UiState};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

/// User-initiated connect waiting for auth success before writing clients.json.
#[derive(Clone)]
struct PendingPair {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    addr: SocketAddr,
    source: ClientSource,
}

/// Commands from UI thread to runtime.
enum UiCommand {
    SaveSettings {
        device_name: String,
        password: String,
        tcp_port: u16,
        udp_port: u16,
        max_payload: u64,
        start_minimized_to_tray: bool,
        auto_start: bool,
    },
    /// Trial dial by IP; persist only after PeerSessionReady.
    AddManual(SocketAddr),
    /// Trial dial discovered device; persist only after auth OK.
    ConnectNearby {
        device_id: Uuid,
        name: String,
        addr: SocketAddr,
    },
    /// Remove from clients.json + disconnect + stop auto-reconnect.
    RemoveClient {
        device_id: Option<Uuid>,
        addr: SocketAddr,
    },
    /// Toggle clipboard mute for a saved client (connection may stay).
    SetClientIgnored {
        device_id: Option<Uuid>,
        addr: SocketAddr,
        ignored: bool,
    },
    /// Re-read clients.json and resync wanted set.
    ReloadClients,
    ClearHistory,
    CopyText(String),
    SetSync(bool),
    /// Immediate language persist (hot-reload already applied in UI thread).
    SetLanguage(String),
}

const TOAST_FRAMES: u32 = 20; // ~4s at 200ms repaint

/// Convenience entry (loads config itself). Prefer `run_with_config` from `main`.
#[allow(dead_code)]
pub fn run() -> Result<()> {
    let force_headless = std::env::args().any(|a| a == "--headless" || a == "-H")
        || std::env::var_os("OHMYCOPY_HEADLESS").is_some();
    let cfg = Config::load_or_create()?;
    let show = cfg.console || force_headless;
    ohmycopy::console_win::set_visible(show);
    run_with_config(cfg, force_headless)
}

/// Entry used by `main` after console visibility has been applied.
///
/// Headless and GUI share this setup (config, engine, history, clipboard, hub,
/// discovery, clipboard watcher). Only the presentation surface forks later:
/// `run_gui` vs `run_headless`. Keep sync/network/config behavior identical.
pub fn run_with_config(cfg_snap: Config, force_headless: bool) -> Result<()> {
    ohmycopy::audit::record(format!(
        "app_start pid={} data_dir={}",
        std::process::id(),
        Config::data_dir()?.display()
    ));
    // Language: config → system locale → English (built-in base is English).
    let lang = ohmycopy::i18n::init_from_config(&cfg_snap.language);
    tracing::info!(language = %lang, "i18n ready");

    let config = Arc::new(Mutex::new(cfg_snap.clone()));

    let engine: SharedEngine = Arc::new(Mutex::new(EngineCore::new(
        cfg_snap.device_id,
        cfg_snap.max_payload_bytes,
        cfg_snap.sync_enabled,
    )));

    let history_path = Config::data_dir()?.join("history.db");
    let history = Arc::new(Mutex::new(HistoryStore::open(
        &history_path,
        cfg_snap.history_limit,
    )?));

    let clipboard = Arc::new(ClipboardService::new()?);
    // Prune stale inbox so received files do not grow without bound.
    if let Err(e) = inbox::cleanup_inbox(
        inbox::INBOX_MAX_TOTAL_BYTES,
        inbox::INBOX_MAX_ENTRIES,
        inbox::INBOX_MAX_AGE,
    ) {
        tracing::warn!(error = %e, "inbox cleanup on startup");
    }

    let mut initial_ui = UiState::default();
    initial_ui.device_name = cfg_snap.device_name.clone();
    initial_ui.password = cfg_snap.password.clone();
    initial_ui.tcp_port = cfg_snap.tcp_port.to_string();
    initial_ui.udp_port = cfg_snap.udp_port.to_string();
    initial_ui.max_payload_mb = format!("{}", cfg_snap.max_payload_bytes / (1024 * 1024));
    initial_ui.sync_enabled = cfg_snap.sync_enabled;
    initial_ui.start_minimized_to_tray = cfg_snap.start_minimized_to_tray;
    initial_ui.auto_start = cfg_snap.auto_start;
    initial_ui.language = lang;
    initial_ui.history = history.lock().list("", 100).unwrap_or_default();
    initial_ui.status_line =
        ohmycopy::i18n::t_args("app.local_device", &[("name", &cfg_snap.device_name)]);
    let ui_shared = Arc::new(Mutex::new(initial_ui));

    // Keep OS login item in sync with config (e.g. after migrate / manual json edit).
    if let Err(e) = ohmycopy::autostart::apply(cfg_snap.auto_start) {
        tracing::warn!(error = %e, enabled = cfg_snap.auto_start, "apply autostart on startup");
    }

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<UiCommand>();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ohmycopy-tokio")
        .build()?;
    // Enter runtime on this thread so nested tokio APIs during setup are valid.
    let _rt_enter = runtime.enter();
    let rt_handle = runtime.handle().clone();

    let hub = Arc::new(NetworkHub::new(
        cfg_snap.device_id,
        cfg_snap.device_name.clone(),
        &cfg_snap.password,
        engine.clone(),
        cfg_snap.tcp_port,
        rt_handle.clone(),
    )?);

    // clients.json — only user-paired devices (migrate legacy manual_peers).
    let clients = Arc::new(Mutex::new(ClientsFile::load_or_create()?));
    // device_id → pending user trial; addr-only trials use pending_by_addr.
    let pending_by_id: Arc<Mutex<HashMap<Uuid, PendingPair>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let pending_by_addr: Arc<Mutex<HashMap<SocketAddr, PendingPair>>> =
        Arc::new(Mutex::new(HashMap::new()));
    {
        let mut cf = clients.lock();
        if !cfg_snap.manual_peers.is_empty() && cf.import_manual_peers(&cfg_snap.manual_peers) {
            let _ = cf.save();
            // Clear migrated peers from config.json
            let mut cfg = config.lock();
            cfg.manual_peers.clear();
            let _ = cfg.save();
        }
        apply_clients(&hub, &cf);
        if let Some(mut u) = ui_shared.try_lock() {
            u.saved_clients = cf.clients.clone();
        }
    }

    let (shutdown_tcp_tx, shutdown_tcp_rx) = mpsc::channel::<()>(1);
    let (shutdown_disc_tx, shutdown_disc_rx) = mpsc::channel::<()>(1);

    {
        let hub_run = Arc::clone(&hub);
        let listen = cfg_snap.listen_addr();
        rt_handle.spawn(async move {
            hub_run.run(listen, shutdown_tcp_rx).await;
        });
    }

    {
        let disc = DiscoveryService::new(
            cfg_snap.device_id,
            cfg_snap.device_name.clone(),
            cfg_snap.tcp_port,
            cfg_snap.udp_port,
            cfg_snap.discover_interval_secs,
        );
        let devices = disc.devices_handle();
        let known_targets = disc.known_targets_handle();
        // Seed unicast discovery targets from clients.json (helps when broadcast is blocked).
        {
            let cf = clients.lock();
            let addrs: Vec<SocketAddr> =
                cf.clients.iter().filter_map(|c| c.socket_addr()).collect();
            // known_targets is tokio RwLock — set via blocking write after spawn.
            let kt = Arc::clone(&known_targets);
            rt_handle.spawn(async move {
                *kt.write().await = addrs;
            });
        }
        let hub_d = Arc::clone(&hub);
        let ui_d = Arc::clone(&ui_shared);
        let clients_d = Arc::clone(&clients);
        let rt = rt_handle.clone();
        rt_handle.spawn(async move {
            let feeder = rt.spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let list = devices.read().await.clone();
                    let mut nearby = Vec::new();
                    let mut unicast_targets = Vec::new();
                    {
                        let cf = clients_d.lock();
                        for d in &list {
                            hub_d.note_peer(d.device_id, d.name.clone(), d.addr);
                            // Discovery never writes clients.json. Only show peers
                            // that are not already saved (removed peers reappear here).
                            let already = cf.contains_device(d.device_id)
                                || cf.contains_addr(&d.addr.to_string());
                            if !already {
                                nearby.push(NearbyDevice {
                                    name: d.name.clone(),
                                    device_id: d.device_id.to_string(),
                                    addr: d.addr.to_string(),
                                });
                            }
                        }
                        // Unicast announce only to saved clients.
                        for c in &cf.clients {
                            if let Some(a) = c.socket_addr() {
                                unicast_targets.push(a);
                            }
                        }
                        if let Some(mut u) = ui_d.try_lock() {
                            u.nearby = nearby;
                            u.saved_clients = cf.clients.clone();
                        } else {
                            let mut u = ui_d.lock();
                            u.nearby = nearby;
                            u.saved_clients = cf.clients.clone();
                        }
                    }
                    *known_targets.write().await = unicast_targets;
                }
            });
            disc.run(shutdown_disc_rx).await;
            feeder.abort();
        });
    }

    // Net events → clipboard / UI / pair-on-success
    {
        let mut ev_rx = hub.subscribe_events();
        let clip = Arc::clone(&clipboard);
        let hist = Arc::clone(&history);
        let ui_s = Arc::clone(&ui_shared);
        let clients_e = Arc::clone(&clients);
        let hub_e = Arc::clone(&hub);
        let pend_id = Arc::clone(&pending_by_id);
        let pend_addr = Arc::clone(&pending_by_addr);
        rt_handle.spawn(async move {
            loop {
                match ev_rx.recv().await {
                    Ok(NetEvent::ClipboardFromRemote(ev)) => {
                        match ev.kind {
                            ContentKind::Text => {
                                let Ok(text) = String::from_utf8(ev.payload.clone()) else {
                                    tracing::warn!("remote text is not utf-8");
                                    continue;
                                };
                                // Always record history even if OS clipboard is down
                                // (common on Linux headless without a live DISPLAY).
                                if let Err(e) = clip.set_text_from_sync(&text) {
                                    tracing::warn!(
                                        error = %e,
                                        "write clipboard text (content still stored in history / last_clip)"
                                    );
                                }
                                let preview = {
                                    let h = hist.lock();
                                    let _ = h.insert_text(
                                        ev.event_id,
                                        ev.source_id,
                                        &text,
                                        ev.created_at,
                                    );
                                    h.list("", 100).unwrap_or_default()
                                };
                                let mut u = ui_s.lock();
                                u.history = preview;
                                u.toast = Some(ohmycopy::i18n::t("toast.text_received"));
                            }
                            ContentKind::File => {
                                ohmycopy::audit::record(format!(
                                    "app_receive_file event={} name={:?} mime={} bytes={}",
                                    ev.event_id, ev.file_name, ev.mime, ev.payload.len()
                                ));
                                if ev.mime == MIME_MULTI_PATHS_ZIP {
                                    let paths = match inbox::store_multi_paths_zip(&ev.event_id.to_string()[..8], &ev.payload) {
                                        Ok(paths) => paths,
                                        Err(e) => {
                                            tracing::warn!(error = %e, "extract multi-path zip");
                                            ui_s.lock().toast = Some(ohmycopy::i18n::t_args("toast.file_save_fail", &[("error", &e.to_string())]));
                                            continue;
                                        }
                                    };
                                    if let Err(e) = clip.set_files_from_sync(&paths) {
                                        tracing::warn!(error = %e, "set clipboard multi-path files");
                                        ui_s.lock().toast = Some(ohmycopy::i18n::t_args("toast.file_clip_fail", &[("error", &e.to_string())]));
                                    }
                                    let count = paths.len().to_string();
                                    let preview = {
                                        let h = hist.lock();
                                        let _ = h.insert_file(ev.event_id, ev.source_id, &format!("{count} items"), &paths[0].parent().unwrap_or(&paths[0]).to_string_lossy(), ev.payload.len() as u64, ev.created_at);
                                        h.list("", 100).unwrap_or_default()
                                    };
                                    let mut u = ui_s.lock();
                                    u.history = preview;
                                    u.toast = Some(ohmycopy::i18n::t_args("toast.files_received", &[("count", &count)]));
                                    continue;
                                }
                                let name = ev
                                    .file_name
                                    .clone()
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or_else(|| {
                                        format!("file-{}", &ev.event_id.to_string()[..8])
                                    });
                                let prefix = &ev.event_id.to_string()[..8];
                                let is_folder = ev.mime == MIME_DIR_ZIP
                                    || ev.mime == "application/x-ohmycopy-dir-zip";
                                // Folder name may arrive as "Name.zip" — strip for display/extract.
                                let display_name = if is_folder {
                                    name.strip_suffix(".zip").unwrap_or(&name).to_string()
                                } else {
                                    name.clone()
                                };

                                let dest = if is_folder {
                                    match inbox::store_folder_zip(
                                        prefix,
                                        &display_name,
                                        &ev.payload,
                                    ) {
                                        Ok(p) => p,
                                        Err(e) => {
                                            tracing::warn!(error = %e, "extract folder zip");
                                            ui_s.lock().toast =
                                                Some(ohmycopy::i18n::t_args("toast.folder_save_fail", &[("error", &e.to_string())]));
                                            continue;
                                        }
                                    }
                                } else {
                                    match inbox::store_file(prefix, &name, &ev.payload) {
                                        Ok(p) => p,
                                        Err(e) => {
                                            tracing::warn!(error = %e, "store inbox file");
                                            ui_s.lock().toast =
                                                Some(ohmycopy::i18n::t_args("toast.file_save_fail", &[("error", &e.to_string())]));
                                            continue;
                                        }
                                    }
                                };

                                // File events always restore a file-list clipboard entry.  Do
                                // not convert image files into bitmap data: Explorer users expect
                                // a copied .png/.jpg to paste as a file.
                                if let Err(e) =
                                    clip.set_files_from_sync(std::slice::from_ref(&dest))
                                {
                                    ohmycopy::audit::record(format!(
                                        "app_receive_file_clipboard event={} result=err error={}",
                                        ev.event_id, e
                                    ));
                                    tracing::warn!(error = %e, "set clipboard files");
                                    ui_s.lock().toast = Some(ohmycopy::i18n::t_args(
                                        "toast.file_clip_fail",
                                        &[("error", &e.to_string())],
                                    ));
                                }
                                else {
                                    ohmycopy::audit::record(format!(
                                        "app_receive_file_clipboard event={} result=ok dest={}",
                                        ev.event_id, dest.display()
                                    ));
                                }
                                // Pass base name only — history layer formats the list title.
                                let list_name = if is_folder {
                                    ohmycopy::i18n::t_args("toast.folder_label", &[("name", &display_name)])
                                } else {
                                    display_name.clone()
                                };
                                let preview = {
                                    let h = hist.lock();
                                    let _ = h.insert_file(
                                        ev.event_id,
                                        ev.source_id,
                                        &list_name,
                                        &dest.to_string_lossy(),
                                        ev.payload.len() as u64,
                                        ev.created_at,
                                    );
                                    h.list("", 100).unwrap_or_default()
                                };
                                let mut u = ui_s.lock();
                                u.history = preview;
                                u.toast = Some(if is_folder {
                                    ohmycopy::i18n::t_args("toast.folder_received", &[("name", &display_name)])
                                } else {
                                    ohmycopy::i18n::t_args("toast.file_received", &[("name", &display_name)])
                                });
                            }
                            ContentKind::Image => {
                                let (img_w, img_h, rgba) = match png_to_rgba(&ev.payload) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        tracing::warn!(error = %e, "decode remote png");
                                        ui_s.lock().toast =
                                            Some(ohmycopy::i18n::t_args("toast.image_display_fail", &[("error", &e.to_string())]));
                                        continue;
                                    }
                                };
                                let clip_ok = match clip.set_image_from_sync(img_w, img_h, rgba) {
                                    Ok(()) => true,
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            "set clipboard image (content still stored in inbox / last_clip)"
                                        );
                                        false
                                    }
                                };
                                let fname = ev
                                    .file_name
                                    .clone()
                                    .unwrap_or_else(|| {
                                        format!(
                                            "screenshot_{}.png",
                                            chrono::Local::now().format("%Y%m%d_%H%M%S")
                                        )
                                    });
                                let path_str = match inbox::store_file(
                                    &ev.event_id.to_string()[..8],
                                    &fname,
                                    &ev.payload,
                                ) {
                                    Ok(p) => p.to_string_lossy().into_owned(),
                                    Err(e) => {
                                        tracing::warn!(error = %e, "store image inbox");
                                        String::new()
                                    }
                                };
                                let dim = format!("{img_w}×{img_h}");
                                let preview = {
                                    let store = hist.lock();
                                    if !path_str.is_empty() {
                                        let _ = store.insert_image(
                                            ev.event_id,
                                            ev.source_id,
                                            &dim,
                                            &path_str,
                                            ev.payload.len() as u64,
                                            ev.created_at,
                                        );
                                    }
                                    store.list("", 100).unwrap_or_default()
                                };
                                let mut u = ui_s.lock();
                                u.history = preview;
                                u.toast = Some(if clip_ok {
                                    ohmycopy::i18n::t_args("toast.image_received", &[("dim", &dim)])
                                } else {
                                    ohmycopy::i18n::t_args(
                                        "toast.image_clip_fail",
                                        &[(
                                            "error",
                                            "OS clipboard unavailable; saved to inbox/last_clip",
                                        )],
                                    )
                                });
                            }
                        }
                    }
                    Ok(NetEvent::PeerUpdated) => {}
                    Ok(NetEvent::PeerSessionReady {
                        device_id,
                        name,
                        addr,
                        we_dialed,
                    }) => {
                        // Mutual pairing: BOTH sides always write clients.json on auth OK.
                        let trial = {
                            let mut by_id = pend_id.lock();
                            let mut by_addr = pend_addr.lock();
                            by_id
                                .remove(&device_id)
                                .or_else(|| by_addr.remove(&addr))
                        };
                        let source = trial.map(|p| p.source).unwrap_or(ClientSource::Discover);
                        {
                            let mut cf = clients_e.lock();
                            cf.add_paired(Some(device_id), name.clone(), addr, source);
                            let _ = cf.save();
                            hub_e.mark_wanted(Some(device_id), addr);
                            let mut u = ui_s.lock();
                            u.saved_clients = cf.clients.clone();
                            u.nearby.retain(|n| {
                                n.device_id != device_id.to_string()
                                    && n.addr != addr.to_string()
                            });
                            u.toast = Some(ohmycopy::i18n::t_args("toast.device_connected", &[("name", &name)]));
                        }
                        tracing::info!(%device_id, %addr, %name, we_dialed, "mutual pair saved");
                    }
                    Ok(NetEvent::PeerAuthFailed {
                        device_id,
                        name,
                        addr,
                    }) => {
                        let was_trial = {
                            let mut by_id = pend_id.lock();
                            let mut by_addr = pend_addr.lock();
                            by_id.remove(&device_id).is_some() || by_addr.remove(&addr).is_some()
                        };
                        let mut u = ui_s.lock();
                        if was_trial {
                            u.toast = Some(ohmycopy::i18n::t_args(
                                "toast.auth_fail_detail",
                                &[("name", &name)],
                            ));
                        } else {
                            u.toast = Some(ohmycopy::i18n::t_args(
                                "toast.auth_fail_short",
                                &[("name", &name)],
                            ));
                        }
                        let _ = (device_id, addr);
                    }
                    Ok(NetEvent::PeerUnpaired {
                        device_id,
                        name,
                        addr,
                        from_remote,
                    }) => {
                        // Remote asked us to unpair — drop clients entry + session (no re-notify).
                        {
                            let mut cf = clients_e.lock();
                            cf.remove(Some(device_id), &addr.to_string());
                            let _ = cf.save();
                            let mut u = ui_s.lock();
                            u.saved_clients = cf.clients.clone();
                            u.toast = Some(if from_remote {
                                ohmycopy::i18n::t_args("toast.device_removed_peer", &[("name", &name)])
                            } else {
                                ohmycopy::i18n::t_args("toast.device_disconnected", &[("name", &name)])
                            });
                        }
                        hub_e.remove_client_silent(Some(device_id), addr);
                        hub_e.set_ignored(Some(device_id), addr, false);
                        tracing::info!(%device_id, %addr, from_remote, "unpaired");
                    }
                    Ok(NetEvent::Status(s)) => {
                        let mut u = ui_s.lock();
                        if u.status_line != s {
                            u.status_line = s;
                        }
                    }
                    Ok(NetEvent::Toast(msg)) => {
                        let mut u = ui_s.lock();
                        // Mark as pending for UI; AppShell will take it once.
                        u.toast = Some(msg);
                        u.toast_ttl_frames = 0; // UI owns TTL after take
                    }
                    Ok(NetEvent::FirewallHint(h)) => {
                        ui_s.lock().firewall_hint = Some(h);
                    }
                    Err(broadcast_err) => {
                        if matches!(
                            broadcast_err,
                            tokio::sync::broadcast::error::RecvError::Closed
                        ) {
                            break;
                        }
                    }
                }
            }
        });
    }

    {
        let hub_p = Arc::clone(&hub);
        let ui_p = Arc::clone(&ui_shared);
        rt_handle.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                let snaps = hub_p.peer_snapshots().await;
                let summary = hub_p.status_summary();
                let mut u = ui_p.lock();
                // Only write when changed — avoid needless UI dirtying / flicker.
                if u.peers.len() != snaps.len()
                    || u.peers
                        .iter()
                        .zip(snaps.iter())
                        .any(|(a, b)| a.status != b.status || a.addr != b.addr || a.name != b.name)
                {
                    u.peers = snaps;
                }
                if u.status_line != summary {
                    u.status_line = summary;
                }
            }
        });
    }

    // Hot-reload clients.json so hand-edited auto_connect entries are picked up.
    {
        let hub_r = Arc::clone(&hub);
        let clients_r = Arc::clone(&clients);
        let ui_r = Arc::clone(&ui_shared);
        rt_handle.spawn(async move {
            let mut last_mtime = None;
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Ok(path) = ClientsFile::path() else {
                    continue;
                };
                let mtime = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok());
                if mtime == last_mtime {
                    continue;
                }
                last_mtime = mtime;
                if let Ok(fresh) = ClientsFile::load(&path) {
                    apply_clients(&hub_r, &fresh);
                    let mut cf = clients_r.lock();
                    *cf = fresh;
                    if let Some(mut u) = ui_r.try_lock() {
                        u.saved_clients = cf.clients.clone();
                    }
                    tracing::info!("reloaded clients.json");
                }
            }
        });
    }

    let shutdown_flag = Arc::new(AtomicBool::new(false));
    {
        let clip = Arc::clone(&clipboard);
        let eng = Arc::clone(&engine);
        let hub_c = Arc::clone(&hub);
        let hist = Arc::clone(&history);
        let ui_c = Arc::clone(&ui_shared);
        let flag = Arc::clone(&shutdown_flag);
        let local_id = cfg_snap.device_id;
        spawn_clipboard_watcher(
            clip,
            move |content| {
                match content {
                    ClipContent::Empty => {}
                    ClipContent::Text(text) => {
                        let ev = {
                            let mut core = eng.lock();
                            if core.note_oversize_local(text.len() as u64) {
                                drop(core);
                                ui_c.lock().toast = Some(ohmycopy::i18n::t_args(
                                    "toast.text_oversize",
                                    &[("kb", &(text.len() / 1024).to_string())],
                                ));
                                return;
                            }
                            core.on_local_text(&text)
                        };
                        if let Some(ev) = ev {
                            ohmycopy::audit::record(format!(
                                "app_local_text event={} bytes={}",
                                ev.event_id,
                                ev.payload.len()
                            ));
                            let preview = {
                                let h = hist.lock();
                                let _ = h.insert_text(ev.event_id, local_id, &text, ev.created_at);
                                h.list("", 100).unwrap_or_default()
                            };
                            {
                                let mut u = ui_c.lock();
                                u.history = preview;
                                u.toast = Some(ohmycopy::i18n::t("toast.text_synced"));
                            }
                            hub_c.broadcast_clipboard(ev);
                        }
                    }
                    ClipContent::Image {
                        width,
                        height,
                        rgba,
                    } => {
                        let png = match rgba_to_png(width, height, &rgba) {
                            Ok(p) => p,
                            Err(e) => {
                                ui_c.lock().toast = Some(ohmycopy::i18n::t_args(
                                    "toast.image_process_fail",
                                    &[("error", &e.to_string())],
                                ));
                                return;
                            }
                        };
                        let size = png.len() as u64;
                        let max = eng.lock().max_payload_bytes;
                        if size > max {
                            ui_c.lock().toast = Some(ohmycopy::i18n::t_args(
                                "toast.image_oversize",
                                &[("mb", &(size / (1024 * 1024)).to_string())],
                            ));
                            return;
                        }
                        let fname = format!(
                            "screenshot_{}_{}x{}.png",
                            chrono::Local::now().format("%Y%m%d_%H%M%S"),
                            width,
                            height
                        );
                        let ev = {
                            let mut core = eng.lock();
                            core.on_local_image(png.clone(), Some(fname.clone()))
                        };
                        if let Some(ev) = ev {
                            ohmycopy::audit::record(format!(
                                "app_local_image event={} bytes={}",
                                ev.event_id,
                                ev.payload.len()
                            ));
                            // Keep a local copy for history re-copy.
                            let path_str = match inbox::store_file(
                                &ev.event_id.to_string()[..8],
                                &fname,
                                &png,
                            ) {
                                Ok(p) => p.to_string_lossy().into_owned(),
                                Err(_) => String::new(),
                            };
                            let preview = {
                                let h = hist.lock();
                                if !path_str.is_empty() {
                                    let _ = h.insert_image(
                                        ev.event_id,
                                        local_id,
                                        &format!("{width}×{height}"),
                                        &path_str,
                                        size,
                                        ev.created_at,
                                    );
                                }
                                h.list("", 100).unwrap_or_default()
                            };
                            {
                                let mut u = ui_c.lock();
                                u.history = preview;
                                u.toast = Some(ohmycopy::i18n::t_args(
                                    "toast.image_synced",
                                    &[
                                        ("width", &width.to_string()),
                                        ("height", &height.to_string()),
                                    ],
                                ));
                            }
                            hub_c.broadcast_clipboard(ev);
                        }
                    }
                    ClipContent::Files(paths) => {
                        let max = eng.lock().max_payload_bytes;
                        if paths.len() > 1 {
                            let (wire_name, bytes, mime) = match inbox::pack_paths(&paths, max) {
                                Ok(payload) => payload,
                                Err(e) => {
                                    ui_c.lock().toast = Some(ohmycopy::i18n::t_args(
                                        "toast.file_read_fail",
                                        &[("error", &e.to_string())],
                                    ));
                                    return;
                                }
                            };
                            let size = bytes.len() as u64;
                            let count = paths.len().to_string();
                            let ev = {
                                let mut core = eng.lock();
                                core.on_local_file(&wire_name, bytes, mime)
                            };
                            if let Some(ev) = ev {
                                let preview = {
                                    let h = hist.lock();
                                    let _ = h.insert_file(
                                        ev.event_id,
                                        local_id,
                                        &format!("{count} items"),
                                        &paths[0].parent().unwrap_or(&paths[0]).to_string_lossy(),
                                        size,
                                        ev.created_at,
                                    );
                                    h.list("", 100).unwrap_or_default()
                                };
                                let mut u = ui_c.lock();
                                u.history = preview;
                                u.toast = Some(ohmycopy::i18n::t_args(
                                    "toast.files_synced",
                                    &[("count", &count)],
                                ));
                                hub_c.broadcast_clipboard(ev);
                            }
                            return;
                        }
                        for path in paths {
                            let is_dir = path.is_dir();
                            let (wire_name, bytes, mime) = match inbox::pack_path(&path, max) {
                                Ok(t) => t,
                                Err(e) => {
                                    ui_c.lock().toast = Some(ohmycopy::i18n::t_args(
                                        "toast.file_read_fail",
                                        &[("error", &e.to_string())],
                                    ));
                                    continue;
                                }
                            };
                            let size = bytes.len() as u64;
                            let base_name = path
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| {
                                    if is_dir {
                                        "folder".into()
                                    } else {
                                        wire_name.clone()
                                    }
                                });
                            // For folders, store original folder name in file_name (wire is .zip).
                            let event_file_name =
                                if is_dir { base_name.clone() } else { wire_name };
                            let ev = {
                                let mut core = eng.lock();
                                core.on_local_file(&event_file_name, bytes, mime)
                            };
                            if let Some(ev) = ev {
                                ohmycopy::audit::record(format!(
                                    "app_local_file event={} name={} mime={} bytes={} emitted=true",
                                    ev.event_id,
                                    event_file_name,
                                    ev.mime,
                                    ev.payload.len()
                                ));
                                let list_name = if is_dir {
                                    ohmycopy::i18n::t_args(
                                        "toast.folder_label",
                                        &[("name", &base_name)],
                                    )
                                } else {
                                    base_name.clone()
                                };
                                let preview = {
                                    let h = hist.lock();
                                    let _ = h.insert_file(
                                        ev.event_id,
                                        local_id,
                                        &list_name,
                                        &path.to_string_lossy(),
                                        size,
                                        ev.created_at,
                                    );
                                    h.list("", 100).unwrap_or_default()
                                };
                                {
                                    let mut u = ui_c.lock();
                                    u.history = preview;
                                    u.toast = Some(if is_dir {
                                        ohmycopy::i18n::t_args(
                                            "toast.folder_synced",
                                            &[("name", &base_name)],
                                        )
                                    } else {
                                        ohmycopy::i18n::t_args(
                                            "toast.file_synced",
                                            &[("name", &base_name)],
                                        )
                                    });
                                }
                                hub_c.broadcast_clipboard(ev);
                            } else {
                                ohmycopy::audit::record(format!(
                                    "app_local_file name={} emitted=false",
                                    event_file_name
                                ));
                            }
                        }
                    }
                }
            },
            flag,
        );
    }

    {
        let config = Arc::clone(&config);
        let eng = Arc::clone(&engine);
        let hub_c = Arc::clone(&hub);
        let hist = Arc::clone(&history);
        let clip = Arc::clone(&clipboard);
        let ui_s = Arc::clone(&ui_shared);
        let clients_c = Arc::clone(&clients);
        let pend_id_c = Arc::clone(&pending_by_id);
        let pend_addr_c = Arc::clone(&pending_by_addr);
        rt_handle.spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    UiCommand::SaveSettings {
                        device_name,
                        password,
                        tcp_port,
                        udp_port,
                        max_payload,
                        start_minimized_to_tray,
                        auto_start,
                    } => {
                        let (old_tcp, old_udp, old_pass, old_auto) = {
                            let cfg = config.lock();
                            (
                                cfg.tcp_port,
                                cfg.udp_port,
                                cfg.password.clone(),
                                cfg.auto_start,
                            )
                        };
                        let port_changed = old_tcp != tcp_port || old_udp != udp_port;
                        let pass_changed = old_pass != password;
                        let auto_changed = old_auto != auto_start;
                        let save_result = {
                            let mut cfg = config.lock();
                            cfg.device_name = device_name;
                            cfg.password = password.clone();
                            cfg.tcp_port = tcp_port;
                            cfg.udp_port = udp_port;
                            cfg.max_payload_bytes = max_payload;
                            cfg.start_minimized_to_tray = start_minimized_to_tray;
                            cfg.auto_start = auto_start;
                            cfg.save()
                        };
                        if let Err(e) = save_result {
                            ui_s.lock().toast = Some(ohmycopy::i18n::t_args(
                                "toast.save_fail",
                                &[("error", &e.to_string())],
                            ));
                        } else {
                            eng.lock().max_payload_bytes = max_payload;
                            // Password can hot-reload for future handshakes; ports need restart.
                            let mut notes: Vec<String> = Vec::new();
                            if pass_changed {
                                match hub_c.update_password(&password) {
                                    Ok(()) => {
                                        if ohmycopy::config::Config::is_insecure_default_password(
                                            &password,
                                        ) {
                                            notes
                                                .push(ohmycopy::i18n::t("toast.password_insecure"));
                                        } else {
                                            notes.push(ohmycopy::i18n::t("toast.password_updated"));
                                        }
                                    }
                                    Err(e) => notes.push(ohmycopy::i18n::t_args(
                                        "toast.password_update_fail",
                                        &[("error", &e.to_string())],
                                    )),
                                }
                            }
                            if port_changed {
                                notes.push(ohmycopy::i18n::t("toast.port_restart"));
                            }
                            if auto_changed || auto_start {
                                match ohmycopy::autostart::apply(auto_start) {
                                    Ok(()) => notes.push(if auto_start {
                                        ohmycopy::i18n::t("toast.autostart_on")
                                    } else {
                                        ohmycopy::i18n::t("toast.autostart_off")
                                    }),
                                    Err(e) => notes.push(ohmycopy::i18n::t_args(
                                        "toast.autostart_fail",
                                        &[("error", &e.to_string())],
                                    )),
                                }
                            }
                            if notes.is_empty() {
                                notes.push(ohmycopy::i18n::t("toast.settings_saved"));
                            }
                            let mut u = ui_s.lock();
                            u.start_minimized_to_tray = start_minimized_to_tray;
                            u.auto_start = auto_start;
                            u.toast = Some(notes.join("；"));
                        }
                    }
                    UiCommand::AddManual(addr) => {
                        pend_addr_c.lock().insert(
                            addr,
                            PendingPair {
                                name: format!("manual@{}", addr.ip()),
                                addr,
                                source: ClientSource::Manual,
                            },
                        );
                        hub_c.trial_connect_addr(addr);
                    }
                    UiCommand::ConnectNearby {
                        device_id,
                        name,
                        addr,
                    } => {
                        pend_id_c.lock().insert(
                            device_id,
                            PendingPair {
                                name: name.clone(),
                                addr,
                                source: ClientSource::Discover,
                            },
                        );
                        hub_c.trial_connect(device_id, addr);
                    }
                    UiCommand::RemoveClient { device_id, addr } => {
                        // Notify peer (Unpair) then drop local — peer removes us too.
                        {
                            let mut cf = clients_c.lock();
                            cf.remove(device_id, &addr.to_string());
                            let _ = cf.save();
                            ui_s.lock().saved_clients = cf.clients.clone();
                            ui_s.lock().toast = Some(ohmycopy::i18n::t_args(
                                "toast.device_removed",
                                &[("addr", &addr.to_string())],
                            ));
                        }
                        hub_c.remove_client(device_id, addr); // notifies + disconnects
                        hub_c.set_ignored(device_id, addr, false);
                    }
                    UiCommand::SetClientIgnored {
                        device_id,
                        addr,
                        ignored,
                    } => {
                        {
                            let mut cf = clients_c.lock();
                            if cf.set_ignored(device_id, &addr.to_string(), ignored) {
                                let _ = cf.save();
                            }
                            ui_s.lock().saved_clients = cf.clients.clone();
                            ui_s.lock().toast = Some(if ignored {
                                ohmycopy::i18n::t_args(
                                    "toast.ignore_on",
                                    &[("addr", &addr.to_string())],
                                )
                            } else {
                                ohmycopy::i18n::t_args(
                                    "toast.ignore_off",
                                    &[("addr", &addr.to_string())],
                                )
                            });
                        }
                        hub_c.set_ignored(device_id, addr, ignored);
                    }
                    UiCommand::ReloadClients => match ClientsFile::load_or_create() {
                        Ok(fresh) => {
                            apply_clients(&hub_c, &fresh);
                            let mut cf = clients_c.lock();
                            *cf = fresh;
                            ui_s.lock().saved_clients = cf.clients.clone();
                            ui_s.lock().toast = Some(ohmycopy::i18n::t("toast.clients_refreshed"));
                        }
                        Err(e) => {
                            ui_s.lock().toast = Some(ohmycopy::i18n::t_args(
                                "toast.clients_refresh_fail",
                                &[("error", &e.to_string())],
                            ));
                        }
                    },
                    UiCommand::ClearHistory => {
                        let hist_ok = hist.lock().clear().is_ok();
                        let inbox_res = inbox::clear_all();
                        let mut u = ui_s.lock();
                        u.history.clear();
                        u.toast = Some(match (hist_ok, inbox_res) {
                            (true, Ok(())) => ohmycopy::i18n::t("toast.history_cleared"),
                            (true, Err(e)) => ohmycopy::i18n::t_args(
                                "toast.history_clear_inbox_fail",
                                &[("error", &e.to_string())],
                            ),
                            (false, Ok(())) => ohmycopy::i18n::t("toast.inbox_clear_history_fail"),
                            (false, Err(e)) => ohmycopy::i18n::t_args(
                                "toast.clear_fail",
                                &[("error", &e.to_string())],
                            ),
                        });
                    }
                    UiCommand::SetLanguage(code) => {
                        let code = ohmycopy::i18n::normalize_lang_code(&code);
                        let ok = ohmycopy::i18n::set_language(&code);
                        if ok {
                            let mut cfg = config.lock();
                            cfg.language = code.clone();
                            if let Err(e) = cfg.save() {
                                ui_s.lock().toast = Some(ohmycopy::i18n::t_args(
                                    "toast.save_fail",
                                    &[("error", &e.to_string())],
                                ));
                            } else {
                                let name = ohmycopy::i18n::available_languages()
                                    .into_iter()
                                    .find(|(c, _)| c == &code)
                                    .map(|(_, n)| n)
                                    .unwrap_or_else(|| code.clone());
                                let mut u = ui_s.lock();
                                u.language = code;
                                u.toast = Some(ohmycopy::i18n::t_args(
                                    "toast.language_changed",
                                    &[("name", &name)],
                                ));
                            }
                        }
                    }
                    UiCommand::CopyText(text) => {
                        // History re-copy:
                        // - file/image rows store an absolute local path in `content`
                        // - text rows store the raw text (may look like a short file name!)
                        // Only treat as filesystem object when content is an absolute path
                        // that still exists — never promote "readme.md" text to a file.
                        let path = std::path::PathBuf::from(&text);
                        let as_path = ohmycopy::clipboard::content_looks_like_absolute_path(&text)
                            && (path.is_file() || path.is_dir());
                        let result = if as_path && path.is_file() {
                            let is_png = path
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.eq_ignore_ascii_case("png"))
                                .unwrap_or(false);
                            if is_png {
                                match std::fs::read(&path) {
                                    Ok(bytes) => match png_to_rgba(&bytes) {
                                        Ok((w, h, rgba)) => clip
                                            .set_image_local(w, h, rgba)
                                            .map(|_| ohmycopy::i18n::t("toast.image_copied")),
                                        Err(e) => Err(e),
                                    },
                                    Err(e) => Err(anyhow::anyhow!(e)),
                                }
                            } else {
                                // Prefer real file-list MIME; if OS rejects, fall back to path text.
                                match clip.set_files_from_sync(std::slice::from_ref(&path)) {
                                    Ok(()) => Ok(ohmycopy::i18n::t("toast.copied")),
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            "set file list failed; falling back to path as text"
                                        );
                                        clip.set_text_local(&text)
                                            .map(|_| ohmycopy::i18n::t("toast.copied"))
                                    }
                                }
                            }
                        } else if as_path && path.is_dir() {
                            match clip.set_files_from_sync(&[path]) {
                                Ok(()) => Ok(ohmycopy::i18n::t("toast.copied")),
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "set folder list failed; falling back to path as text"
                                    );
                                    clip.set_text_local(&text)
                                        .map(|_| ohmycopy::i18n::t("toast.copied"))
                                }
                            }
                        } else {
                            clip.set_text_local(&text)
                                .map(|_| ohmycopy::i18n::t("toast.copied"))
                        };
                        match result {
                            Ok(msg) => ui_s.lock().toast = Some(msg),
                            Err(e) => {
                                ui_s.lock().toast = Some(ohmycopy::i18n::t_args(
                                    "toast.copy_fail",
                                    &[("error", &e.to_string())],
                                ))
                            }
                        }
                    }
                    UiCommand::SetSync(enabled) => {
                        eng.lock().sync_enabled = enabled;
                        {
                            let mut cfg = config.lock();
                            cfg.sync_enabled = enabled;
                            let _ = cfg.save();
                        }
                        ui_s.lock().sync_enabled = enabled;
                    }
                }
            }
        });
    }

    // Must drop runtime enter-guard before block_on / long waits outside async tasks.
    drop(_rt_enter);

    let result = if force_headless {
        tracing::info!("--headless / OHMYCOPY_HEADLESS set, skip GUI");
        // Ensure console for headless status (in case config.console was false).
        ohmycopy::console_win::set_visible(true);
        run_headless(&hub, &cfg_snap, &shutdown_flag)
    } else {
        let ui_for_eframe = ui_shared.lock().clone();
        let ui_shared_ui = Arc::clone(&ui_shared);
        let cmd_tx_ui = cmd_tx.clone();
        match run_gui(ui_for_eframe, ui_shared_ui, cmd_tx_ui) {
            Ok(()) => Ok(()),
            Err(gui_err) => {
                tracing::error!(error = %gui_err, "all GUI backends failed");
                // Headless needs a visible console for status.
                ohmycopy::console_win::set_visible(true);
                eprintln!();
                eprintln!("{}", ohmycopy::i18n::t("gui_fail.no_adapter"));
                eprintln!(
                    "{}",
                    ohmycopy::i18n::t_args("gui_fail.error", &[("error", &gui_err.to_string())])
                );
                eprintln!("{}", ohmycopy::i18n::t("gui_fail.fallback"));
                eprintln!("{}", ohmycopy::i18n::t("gui_fail.hint"));
                eprintln!();
                run_headless(&hub, &cfg_snap, &shutdown_flag)
            }
        }
    };

    shutdown_flag.store(true, Ordering::SeqCst);
    let _ = shutdown_tcp_tx.try_send(());
    let _ = shutdown_disc_tx.try_send(());
    std::thread::sleep(Duration::from_millis(200));
    runtime.shutdown_timeout(Duration::from_secs(2));

    result
}

/// No-window mode for servers / machines without OpenGL or WARP.
fn run_headless(
    hub: &Arc<NetworkHub>,
    cfg: &Config,
    shutdown_flag: &Arc<AtomicBool>,
) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop_c = Arc::clone(&stop);
        // Best-effort Ctrl+C on a detached thread (works even without GUI message loop).
        std::thread::Builder::new()
            .name("ohmycopy-ctrlc".into())
            .spawn(move || {
                ctrlc_wait();
                stop_c.store(true, Ordering::SeqCst);
            })
            .ok();
    }

    // Headless uses the same hub/clipboard/engine started in run_with_config
    // before this branch — only the presentation surface differs from GUI.
    println!("====================================================");
    println!(
        " {}",
        ohmycopy::i18n::t_args("headless.title", &[("version", env!("CARGO_PKG_VERSION"))],)
    );
    // Warn early when OS clipboard cannot be opened (typical Linux SSH/service).
    match ohmycopy::clipboard::probe_clipboard_available() {
        Ok(()) => {
            println!("  clipboard : OK (OS / CLI backend)");
        }
        Err(e) => {
            println!("  clipboard : UNAVAILABLE — {e}");
            println!("  tip       : network sync/relay still works; local paste needs a desktop");
            println!("              session. Try: export DISPLAY=:0  and/or  apt install xclip");
            println!("              Received text is also saved under ~/.ohmycopy/last_clip/");
        }
    }
    println!("----------------------------------------------------");
    println!(
        " {}",
        ohmycopy::i18n::t_args("headless.device_name", &[("name", &cfg.device_name)])
    );
    println!(
        " {}",
        ohmycopy::i18n::t_args("headless.device_id", &[("id", &cfg.device_id.to_string())])
    );
    println!(
        " {}",
        ohmycopy::i18n::t_args("headless.listen", &[("port", &cfg.tcp_port.to_string())],)
    );
    let sync_state = if cfg.sync_enabled {
        ohmycopy::i18n::t("headless.on")
    } else {
        ohmycopy::i18n::t("headless.off")
    };
    println!(
        " {}",
        ohmycopy::i18n::t_args("headless.sync", &[("state", &sync_state)])
    );
    let work = Config::config_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    println!(
        " {}",
        ohmycopy::i18n::t_args("headless.work_dir", &[("path", &work)])
    );
    println!(" {}", ohmycopy::i18n::t("headless.work_dir_hint"));
    println!("----------------------------------------------------");
    println!(" {}", ohmycopy::i18n::t("headless.running"));
    println!(" {}", ohmycopy::i18n::t("headless.clients_hint"));
    println!(
        " {}",
        ohmycopy::i18n::t_args(
            "headless.connect_hint",
            &[("port", &cfg.tcp_port.to_string())],
        )
    );
    println!(" {}", ohmycopy::i18n::t("headless.ctrl_c"));
    println!("====================================================");

    let mut tick = 0u64;
    while !stop.load(Ordering::SeqCst) && !shutdown_flag.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(2));
        tick += 1;
        if tick.is_multiple_of(5) {
            // every ~10s
            let summary = hub.status_summary();
            let n = hub.connected_count();
            let time = chrono::Local::now().format("%H:%M:%S").to_string();
            println!(
                " {}",
                ohmycopy::i18n::t_args(
                    "headless.tick",
                    &[
                        ("time", &time),
                        ("summary", &summary),
                        ("n", &n.to_string()),
                    ],
                )
            );
        }
    }
    println!(" {}", ohmycopy::i18n::t("headless.exiting"));
    Ok(())
}

fn ctrlc_wait() {
    // Lightweight cross-platform-ish wait: park until process receives interrupt.
    // On Windows, console Ctrl+C ends the process by default unless we set a handler.
    #[cfg(windows)]
    {
        unsafe {
            use std::sync::atomic::{AtomicBool, Ordering};
            static HIT: AtomicBool = AtomicBool::new(false);
            unsafe extern "system" fn handler(_: u32) -> i32 {
                HIT.store(true, Ordering::SeqCst);
                1 // TRUE = handled
            }
            // BOOL SetConsoleCtrlHandler(PHANDLER_ROUTINE, BOOL)
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn SetConsoleCtrlHandler(
                    handler: Option<unsafe extern "system" fn(u32) -> i32>,
                    add: i32,
                ) -> i32;
            }
            SetConsoleCtrlHandler(Some(handler), 1);
            while !HIT.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    #[cfg(not(windows))]
    {
        // Unix: block on SIGINT via signal_hook-less busy wait using libc is heavy;
        // simply sleep until killed — user closes terminal or sends signal to process.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}

/// Sync hub wanted set with clients.json and dial every saved client.
fn apply_clients(hub: &Arc<NetworkHub>, clients: &ClientsFile) {
    let entries = clients.as_connect_entries();
    hub.sync_from_clients(&entries);
    hub.sync_ignored(clients.ignored_device_ids(), clients.ignored_addrs());
    for c in clients.all_connect_list() {
        tracing::info!(
            name = %c.name,
            addr = %c.addr,
            ignored = c.ignored,
            "auto-connect from clients.json"
        );
    }
}

fn viewport_builder(start_minimized_to_tray: bool) -> egui::ViewportBuilder {
    let mut vb = egui::ViewportBuilder::default()
        // Tall enough for Settings (fields + checkboxes + action buttons) plus
        // the bottom status/firewall bar without clipping the last controls.
        .with_inner_size([720.0, 660.0])
        .with_min_inner_size([480.0, 420.0])
        .with_title(tray::WINDOW_TITLE);
    if let Ok(icon) = ohmycopy::icon::egui_icon_data() {
        vb = vb.with_icon(icon);
    }
    if start_minimized_to_tray {
        // Reduce first-frame flash on Windows: invisible, not focused, no taskbar,
        // and create far off-screen until we hide via Win32.
        vb = vb
            .with_visible(false)
            .with_active(false)
            .with_taskbar(false)
            .with_position(egui::pos2(-32000.0, -32000.0));
    }
    vb
}

/// Pick any usable GPU adapter; prefer discrete/integrated, then CPU/WARP software.
fn select_wgpu_adapter(
    adapters: &[eframe::wgpu::Adapter],
    surface: Option<&eframe::wgpu::Surface<'_>>,
) -> Result<eframe::wgpu::Adapter, String> {
    let compatible =
        |a: &eframe::wgpu::Adapter| surface.map(|s| a.is_surface_supported(s)).unwrap_or(true);

    let mut infos: Vec<String> = Vec::new();
    for a in adapters {
        let i = a.get_info();
        infos.push(format!("{} ({:?}/{:?})", i.name, i.backend, i.device_type));
    }
    tracing::info!(adapters = ?infos, "wgpu adapters");

    let is_gpu = |a: &eframe::wgpu::Adapter| {
        matches!(
            a.get_info().device_type,
            eframe::wgpu::DeviceType::DiscreteGpu | eframe::wgpu::DeviceType::IntegratedGpu
        )
    };
    let is_soft = |a: &eframe::wgpu::Adapter| {
        matches!(
            a.get_info().device_type,
            eframe::wgpu::DeviceType::Cpu | eframe::wgpu::DeviceType::VirtualGpu
        )
    };

    // On Windows prefer DX12 first — avoids Vulkan extension present-mode warnings
    // (e.g. "Unrecognized present mode 1000361000") seen on some NVIDIA drivers.
    #[cfg(windows)]
    {
        for a in adapters {
            if a.get_info().backend == eframe::wgpu::Backend::Dx12 && is_gpu(a) && compatible(a) {
                tracing::info!(name = %a.get_info().name, backend = "dx12", "selected GPU adapter");
                return Ok(a.clone());
            }
        }
        for a in adapters {
            if a.get_info().backend == eframe::wgpu::Backend::Dx12 && is_soft(a) && compatible(a) {
                tracing::info!(name = %a.get_info().name, backend = "dx12", "selected WARP adapter");
                return Ok(a.clone());
            }
        }
    }

    for a in adapters {
        if is_gpu(a) && compatible(a) {
            tracing::info!(name = %a.get_info().name, "selected GPU adapter");
            return Ok(a.clone());
        }
    }
    for a in adapters {
        if is_soft(a) && compatible(a) {
            tracing::info!(name = %a.get_info().name, "selected software adapter");
            return Ok(a.clone());
        }
    }
    if let Some(a) = adapters.iter().find(|a| compatible(a)) {
        tracing::info!(name = %a.get_info().name, "selected fallback adapter");
        return Ok(a.clone());
    }
    Err(ohmycopy::i18n::t_args(
        "gui_fail.no_adapter_listed",
        &[("list", &infos.join(", "))],
    ))
}

fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    use std::sync::Arc;

    let mut create = WgpuSetupCreateNew::default();
    // Enumerate as many backends as possible so WARP / soft adapters can appear.
    create.instance_descriptor.backends = eframe::wgpu::Backends::all();
    create.instance_descriptor.flags = eframe::wgpu::InstanceFlags::default()
        .union(eframe::wgpu::InstanceFlags::ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER);
    create.power_preference = eframe::wgpu::PowerPreference::LowPower;
    create.native_adapter_selector = Some(Arc::new(select_wgpu_adapter));

    WgpuConfiguration {
        // Stable FIFO — avoids "Unrecognized present mode 1000361000" on some drivers.
        present_mode: eframe::wgpu::PresentMode::Fifo,
        wgpu_setup: WgpuSetup::CreateNew(create),
        ..Default::default()
    }
}

fn run_gui(
    ui_for_eframe: UiState,
    ui_shared_ui: Arc<Mutex<UiState>>,
    cmd_tx_ui: mpsc::UnboundedSender<UiCommand>,
) -> Result<()> {
    let start_hidden = ui_for_eframe.start_minimized_to_tray;
    // Kill the common Windows flash: winit may still briefly map the window.
    if start_hidden {
        tray::spawn_startup_hide_guard(tray::WINDOW_TITLE);
    }
    let make_app =
        |ui_state: UiState, shared: Arc<Mutex<UiState>>, tx: mpsc::UnboundedSender<UiCommand>| {
            move |cc: &eframe::CreationContext<'_>| {
                let true_quit = Arc::new(AtomicBool::new(false));
                let sync0 = ui_state.sync_enabled;
                let start_min = ui_state.start_minimized_to_tray;
                let tray = match AppTray::new(sync0, Arc::clone(&true_quit)) {
                    Ok(t) => {
                        // Apply sync from tray menu immediately (engine loop), even if
                        // egui `update` is suspended while the window is hidden.
                        let tx_sync = tx.clone();
                        let shared_sync = Arc::clone(&shared);
                        t.set_on_sync(Arc::new(move |enabled: bool| {
                            if let Some(mut u) = shared_sync.try_lock() {
                                u.sync_enabled = enabled;
                            }
                            let _ = tx_sync.send(UiCommand::SetSync(enabled));
                        }));
                        Some(t)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "system tray unavailable");
                        None
                    }
                };
                // Hide as early as CreationContext (before first paint when possible).
                if start_min {
                    cc.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    cc.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                            -32000.0, -32000.0,
                        )));
                    let _ = tray::win32_set_window_visible_quiet(tray::WINDOW_TITLE, false);
                }
                Ok(Box::new(AppShell {
                    inner: OhMyCopyApp::new(cc, ui_state),
                    ui_shared: shared,
                    cmd_tx: tx,
                    tray,
                    true_quit,
                    // Keep re-hiding for several frames (backends re-show during init).
                    hide_on_start_frames: if start_min { 30 } else { 0 },
                }) as Box<dyn eframe::App>)
            }
        };

    // --- Attempt 1: wgpu (DX12 / WARP / Vulkan) ---
    {
        let options = eframe::NativeOptions {
            viewport: viewport_builder(start_hidden),
            renderer: eframe::Renderer::Wgpu,
            hardware_acceleration: eframe::HardwareAcceleration::Preferred,
            wgpu_options: wgpu_options(),
            vsync: true,
            centered: !start_hidden,
            ..Default::default()
        };
        tracing::info!(start_hidden, "trying GUI backend: wgpu");
        match eframe::run_native(
            tray::WINDOW_TITLE,
            options,
            Box::new(make_app(
                ui_for_eframe.clone(),
                Arc::clone(&ui_shared_ui),
                cmd_tx_ui.clone(),
            )),
        ) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "wgpu backend failed, trying OpenGL (glow)…");
            }
        }
    }

    // --- Attempt 2: glow OpenGL, software preferred ---
    {
        let options = eframe::NativeOptions {
            viewport: viewport_builder(start_hidden),
            renderer: eframe::Renderer::Glow,
            hardware_acceleration: eframe::HardwareAcceleration::Off,
            vsync: true,
            centered: !start_hidden,
            ..Default::default()
        };
        tracing::info!("trying GUI backend: glow (OpenGL, hw accel off)");
        match eframe::run_native(
            tray::WINDOW_TITLE,
            options,
            Box::new(make_app(
                ui_for_eframe.clone(),
                Arc::clone(&ui_shared_ui),
                cmd_tx_ui.clone(),
            )),
        ) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "glow (Off) failed, trying glow Preferred…");
            }
        }
    }

    // --- Attempt 3: glow default preferred ---
    {
        let options = eframe::NativeOptions {
            viewport: viewport_builder(start_hidden),
            renderer: eframe::Renderer::Glow,
            hardware_acceleration: eframe::HardwareAcceleration::Preferred,
            vsync: true,
            centered: !start_hidden,
            ..Default::default()
        };
        tracing::info!("trying GUI backend: glow (OpenGL preferred)");
        eframe::run_native(
            tray::WINDOW_TITLE,
            options,
            Box::new(make_app(ui_for_eframe, ui_shared_ui, cmd_tx_ui)),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "{}",
                ohmycopy::i18n::t_args("gui_fail.detail", &[("error", &e.to_string())])
            )
        })
    }
}

struct AppShell {
    inner: OhMyCopyApp,
    ui_shared: Arc<Mutex<UiState>>,
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
    tray: Option<AppTray>,
    /// When true, window close actually exits (tray "退出").
    true_quit: Arc<AtomicBool>,
    /// Frames left to re-apply hide (start minimized to tray; kills first-frame flash).
    hide_on_start_frames: u32,
}

fn peers_eq(
    a: &[ohmycopy::net::peer::PeerSnapshot],
    b: &[ohmycopy::net::peer::PeerSnapshot],
) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.device_id == y.device_id
                && x.name == y.name
                && x.addr == y.addr
                && x.status == y.status
                && x.connected == y.connected
                && x.connecting == y.connecting
                && x.last_error == y.last_error
        })
}

fn nearby_eq(a: &[NearbyDevice], b: &[NearbyDevice]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.name == y.name && x.device_id == y.device_id && x.addr == y.addr)
}

fn history_eq(a: &[ohmycopy::history::HistoryItem], b: &[ohmycopy::history::HistoryItem]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.event_id == y.event_id && x.preview == y.preview && x.content == y.content
        })
}

impl eframe::App for AppShell {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Bind egui context so tray/menu handlers can wake this loop while hidden.
        if let Some(tray) = &self.tray {
            tray.bind_egui_ctx(ctx);
        }

        // Always schedule a soft repaint so tray actions still get drained if
        // the window is hidden (eframe often stops updating when invisible).
        ctx.request_repaint_after(std::time::Duration::from_millis(400));

        // Start minimized to tray: keep re-hiding for several frames so winit/eframe
        // cannot flash the main window during first paint / adapter setup.
        if self.hide_on_start_frames > 0 {
            self.hide_on_start_frames -= 1;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                -32000.0, -32000.0,
            )));
            let _ = tray::win32_set_window_visible_quiet(tray::WINDOW_TITLE, false);
            if self.hide_on_start_frames == 0 {
                tracing::info!("start_minimized_to_tray: main window hidden, tray only");
            }
        }

        // --- Close to tray (X hides; tray "退出" quits) ---
        let want_quit = self.true_quit.load(Ordering::SeqCst)
            || self
                .tray
                .as_ref()
                .map(|t| t.is_true_quit())
                .unwrap_or(false);
        if ctx.input(|i| i.viewport().close_requested()) && !want_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            tray::win32_set_window_visible(tray::WINDOW_TITLE, false);
            if let Some(tray) = &self.tray {
                tray.set_sync_checked(self.inner.ui.sync_enabled);
            }
        }

        // --- Tray menu / click actions (show/quit also applied in tray callbacks) ---
        if let Some(tray) = &self.tray {
            for action in tray.drain_actions() {
                match action {
                    TrayAction::ShowWindow => {
                        // Cancel any remaining startup-hide frames.
                        self.hide_on_start_frames = 0;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        // Center-ish restore; Win32 SHOW will place it back on-screen.
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                            120.0, 80.0,
                        )));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        tray::win32_set_window_visible(tray::WINDOW_TITLE, true);
                        ctx.request_repaint();
                    }
                    TrayAction::Quit => {
                        self.true_quit.store(true, Ordering::SeqCst);
                        tray.set_true_quit(true);
                        tray::win32_set_window_visible(tray::WINDOW_TITLE, true);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    TrayAction::ToggleSync => {
                        // Already applied in menu handler via on_sync; resync UI + menu text.
                        let enabled = tray.sync_enabled();
                        self.inner.ui.sync_enabled = enabled;
                        tray.set_sync_checked(enabled);
                        if let Some(mut shared) = self.ui_shared.try_lock() {
                            shared.sync_enabled = enabled;
                        }
                    }
                }
            }
        }

        // Pull shared snapshot once; never re-apply the same toast (that caused 闪烁).
        if let Some(mut shared) = self.ui_shared.try_lock() {
            if self.inner.ui.status_line != shared.status_line {
                self.inner.ui.status_line = shared.status_line.clone();
            }
            if self.inner.ui.firewall_hint != shared.firewall_hint {
                self.inner.ui.firewall_hint = shared.firewall_hint.clone();
            }
            // Peers / nearby / history: replace only when content changed.
            if !peers_eq(&self.inner.ui.peers, &shared.peers) {
                self.inner.ui.peers = shared.peers.clone();
            }
            if !nearby_eq(&self.inner.ui.nearby, &shared.nearby) {
                self.inner.ui.nearby = shared.nearby.clone();
            }
            // Refresh clients when identity OR ignore/sync flags change.
            if self.inner.ui.saved_clients.len() != shared.saved_clients.len()
                || self
                    .inner
                    .ui
                    .saved_clients
                    .iter()
                    .zip(shared.saved_clients.iter())
                    .any(|(a, b)| {
                        a.addr != b.addr
                            || a.auto_connect != b.auto_connect
                            || a.name != b.name
                            || a.ignored != b.ignored
                            || a.device_id != b.device_id
                    })
            {
                self.inner.ui.saved_clients = shared.saved_clients.clone();
            }
            if !history_eq(&self.inner.ui.history, &shared.history) {
                self.inner.ui.history = shared.history.clone();
            }
            // Take toast from backend exactly once (move out of shared).
            if let Some(t) = shared.toast.take() {
                self.inner.ui.toast = Some(t);
                self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
            }
            self.inner.ui.sync_enabled = shared.sync_enabled;
        }

        self.inner.ui.cmd_save_settings = false;
        self.inner.ui.cmd_open_config_folder = false;
        self.inner.ui.cmd_add_manual = false;
        self.inner.ui.cmd_connect_nearby = None;
        self.inner.ui.cmd_remove_client = None;
        self.inner.ui.cmd_set_ignore = None;
        // confirm_remove is sticky until user acts in the dialog.
        self.inner.ui.cmd_reload_clients = false;
        self.inner.ui.cmd_clear_history = false;
        self.inner.ui.cmd_copy_history = None;
        self.inner.ui.cmd_toggle_sync = false;
        self.inner.ui.cmd_set_language = None;

        // Linux tray-icon/muda needs occasional GTK main-loop pumps.
        tray::poll_gtk_events();

        let sync_before = self.inner.ui.sync_enabled;
        self.inner.update(ctx, frame);

        // Keep tray checkbox aligned with UI toggle.
        if self.inner.ui.sync_enabled != sync_before {
            if let Some(tray) = &self.tray {
                tray.set_sync_checked(self.inner.ui.sync_enabled);
            }
        }

        if let Some(code) = self.inner.ui.cmd_set_language.take() {
            // UI already called set_language for immediate hot-reload; persist + toast.
            let _ = self.cmd_tx.send(UiCommand::SetLanguage(code));
            if let Some(tray) = &self.tray {
                tray.refresh_i18n_labels();
                tray.set_sync_checked(self.inner.ui.sync_enabled);
            }
        }

        if self.inner.ui.cmd_toggle_sync {
            let _ = self
                .cmd_tx
                .send(UiCommand::SetSync(self.inner.ui.sync_enabled));
            if let Some(tray) = &self.tray {
                tray.set_sync_checked(self.inner.ui.sync_enabled);
            }
        }
        if self.inner.ui.cmd_save_settings {
            let tcp: u16 = self.inner.ui.tcp_port.parse().unwrap_or(3721);
            let udp: u16 = self.inner.ui.udp_port.parse().unwrap_or(3721);
            let mb: u64 = self.inner.ui.max_payload_mb.parse().unwrap_or(10);
            let _ = self.cmd_tx.send(UiCommand::SaveSettings {
                device_name: self.inner.ui.device_name.clone(),
                password: self.inner.ui.password.clone(),
                tcp_port: tcp,
                udp_port: udp,
                max_payload: mb * 1024 * 1024,
                start_minimized_to_tray: self.inner.ui.start_minimized_to_tray,
                auto_start: self.inner.ui.auto_start,
            });
        }
        // Note: cmd_open_config_folder is cleared at frame start; capture after update.
        if self.inner.ui.cmd_open_config_folder {
            match Config::open_config_folder() {
                Ok(()) => {
                    self.inner.ui.toast = Some(ohmycopy::i18n::t("toast.data_folder_opened"));
                    self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
                }
                Err(e) => {
                    self.inner.ui.toast = Some(ohmycopy::i18n::t_args(
                        "toast.data_folder_fail",
                        &[("error", &e.to_string())],
                    ));
                    self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
                }
            }
        }
        if self.inner.ui.cmd_add_manual {
            if let Ok(addr) = self.inner.ui.manual_addr.parse::<SocketAddr>() {
                let _ = self.cmd_tx.send(UiCommand::AddManual(addr));
            } else {
                self.inner.ui.toast = Some(ohmycopy::i18n::t("toast.bad_manual_addr"));
                self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
            }
        }
        if let Some((id_str, name, addr_str)) = self.inner.ui.cmd_connect_nearby.take() {
            match (id_str.parse::<Uuid>(), addr_str.parse::<SocketAddr>()) {
                (Ok(device_id), Ok(addr)) => {
                    let _ = self.cmd_tx.send(UiCommand::ConnectNearby {
                        device_id,
                        name,
                        addr,
                    });
                }
                _ => {
                    self.inner.ui.toast = Some(ohmycopy::i18n::t("toast.bad_device_addr"));
                    self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
                }
            }
        }
        if let Some((device_id, addr_str)) = self.inner.ui.cmd_remove_client.take() {
            if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                let _ = self
                    .cmd_tx
                    .send(UiCommand::RemoveClient { device_id, addr });
            } else {
                self.inner.ui.toast = Some(ohmycopy::i18n::t("toast.bad_device_addr_short"));
                self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
            }
        }
        if let Some((device_id, addr_str, ignored)) = self.inner.ui.cmd_set_ignore.take() {
            if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                // Keep shared snapshot in sync immediately so the next-frame pull
                // does not revert the optimistic UI flip before the async worker runs.
                if let Some(mut s) = self.ui_shared.try_lock() {
                    for c in &mut s.saved_clients {
                        if (device_id.is_some() && c.device_id == device_id) || c.addr == addr_str {
                            c.ignored = ignored;
                        }
                    }
                }
                let _ = self.cmd_tx.send(UiCommand::SetClientIgnored {
                    device_id,
                    addr,
                    ignored,
                });
            }
        }
        if self.inner.ui.cmd_reload_clients {
            let _ = self.cmd_tx.send(UiCommand::ReloadClients);
        }
        if self.inner.ui.cmd_clear_history {
            let _ = self.cmd_tx.send(UiCommand::ClearHistory);
        }
        if let Some(text) = self.inner.ui.cmd_copy_history.take() {
            let _ = self.cmd_tx.send(UiCommand::CopyText(text));
        }
    }
}
