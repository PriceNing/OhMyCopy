use ohmycopy::clients::{ClientSource, ClientsFile};
use ohmycopy::clipboard::{
    png_to_rgba, rgba_to_png, spawn_clipboard_watcher, ClipContent, ClipboardService,
};
use ohmycopy::inbox::{self, MIME_DIR_ZIP};
use ohmycopy::protocol::ContentKind;
use ohmycopy::config::Config;
use ohmycopy::engine::{EngineCore, SharedEngine};
use ohmycopy::history::HistoryStore;
use ohmycopy::net::discover::DiscoveryService;
use ohmycopy::net::tcp::{NetEvent, NetworkHub};
use ohmycopy::tray::{self, AppTray, TrayAction};
use ohmycopy::ui::{NearbyDevice, OhMyCopyApp, UiState};
use anyhow::Result;
use eframe::egui;
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
pub fn run_with_config(cfg_snap: Config, force_headless: bool) -> Result<()> {
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
    initial_ui.history = history.lock().list("", 100).unwrap_or_default();
    initial_ui.status_line = format!("本机：{}", cfg_snap.device_name);
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
        if !cfg_snap.manual_peers.is_empty() {
            if cf.import_manual_peers(&cfg_snap.manual_peers) {
                let _ = cf.save();
                // Clear migrated peers from config.json
                let mut cfg = config.lock();
                cfg.manual_peers.clear();
                let _ = cfg.save();
            }
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
            let addrs: Vec<SocketAddr> = cf
                .clients
                .iter()
                .filter_map(|c| c.socket_addr())
                .collect();
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
                                if let Err(e) = clip.set_text_from_sync(&text) {
                                    tracing::warn!(error = %e, "write clipboard text");
                                    continue;
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
                                u.toast = Some("已收到对方的文字".into());
                            }
                            ContentKind::File => {
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
                                                Some(format!("收到文件夹，但保存失败：{e}"));
                                            continue;
                                        }
                                    }
                                } else {
                                    match inbox::store_file(prefix, &name, &ev.payload) {
                                        Ok(p) => p,
                                        Err(e) => {
                                            tracing::warn!(error = %e, "store inbox file");
                                            ui_s.lock().toast =
                                                Some(format!("收到文件，但保存失败：{e}"));
                                            continue;
                                        }
                                    }
                                };

                                // Prefer bitmap paste for image files (WeChat etc. expect image, not path).
                                let looks_img = !is_folder
                                    && dest
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .map(|e| {
                                            matches!(
                                                e.to_ascii_lowercase().as_str(),
                                                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
                                            )
                                        })
                                        .unwrap_or(false);
                                if looks_img {
                                    match std::fs::read(&dest)
                                        .ok()
                                        .and_then(|b| image::load_from_memory(&b).ok())
                                    {
                                        Some(img) => {
                                            let rgba = img.to_rgba8();
                                            let (w, h) = rgba.dimensions();
                                            if let Err(e) =
                                                clip.set_image_from_sync(w, h, rgba.into_raw())
                                            {
                                                tracing::warn!(error = %e, "set image from file");
                                                let _ = clip.set_files_from_sync(&[dest.clone()]);
                                            }
                                        }
                                        None => {
                                            if let Err(e) =
                                                clip.set_files_from_sync(&[dest.clone()])
                                            {
                                                tracing::warn!(error = %e, "set clipboard files");
                                                ui_s.lock().toast = Some(format!(
                                                    "文件已保存，但复制到剪贴板失败：{e}"
                                                ));
                                            }
                                        }
                                    }
                                } else if let Err(e) = clip.set_files_from_sync(&[dest.clone()]) {
                                    tracing::warn!(error = %e, "set clipboard files");
                                    ui_s.lock().toast =
                                        Some(format!("文件已保存，但复制到剪贴板失败：{e}"));
                                }
                                // Pass base name only — history layer formats the list title.
                                let list_name = if is_folder {
                                    format!("文件夹 {display_name}")
                                } else {
                                    display_name.clone()
                                };
                                let preview = {
                                    let h = hist.lock();
                                    if looks_img && !is_folder {
                                        let _ = h.insert_image(
                                            ev.event_id,
                                            ev.source_id,
                                            &display_name,
                                            &dest.to_string_lossy(),
                                            ev.payload.len() as u64,
                                            ev.created_at,
                                        );
                                    } else {
                                        let _ = h.insert_file(
                                            ev.event_id,
                                            ev.source_id,
                                            &list_name,
                                            &dest.to_string_lossy(),
                                            ev.payload.len() as u64,
                                            ev.created_at,
                                        );
                                    }
                                    h.list("", 100).unwrap_or_default()
                                };
                                let mut u = ui_s.lock();
                                u.history = preview;
                                u.toast = Some(if is_folder {
                                    format!("已收到文件夹：{display_name}")
                                } else {
                                    format!("已收到文件：{display_name}")
                                });
                            }
                            ContentKind::Image => {
                                let (img_w, img_h, rgba) = match png_to_rgba(&ev.payload) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        tracing::warn!(error = %e, "decode remote png");
                                        ui_s.lock().toast =
                                            Some(format!("图片无法显示：{e}"));
                                        continue;
                                    }
                                };
                                if let Err(e) = clip.set_image_from_sync(img_w, img_h, rgba) {
                                    tracing::warn!(error = %e, "set clipboard image");
                                    ui_s.lock().toast =
                                        Some(format!("图片复制到剪贴板失败：{e}"));
                                    continue;
                                }
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
                                u.toast = Some(format!("已收到图片（{dim}）"));
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
                        let source = trial
                            .map(|p| p.source)
                            .unwrap_or(if we_dialed {
                                ClientSource::Discover
                            } else {
                                ClientSource::Discover
                            });
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
                            u.toast = Some(format!("已连接「{name}」，可以同步了"));
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
                            u.toast = Some(format!(
                                "密码不正确，未能连接「{name}」。请确认双方密码一致。"
                            ));
                        } else {
                            u.toast = Some(format!("与「{name}」密码不一致，连接失败"));
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
                                format!("「{name}」已断开，并从列表中移除")
                            } else {
                                format!("已与「{name}」断开连接")
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
                let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
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
                                let max = core.max_payload_bytes;
                                drop(core);
                                ui_c.lock().toast = Some(format!(
                                    "这段文字太大（约 {} KB），超过本机上限，未同步。可在设置中提高「单次同步上限」。",
                                    text.len() / 1024
                                ));
                                return;
                            }
                            core.on_local_text(&text)
                        };
                        if let Some(ev) = ev {
                            let preview = {
                                let h = hist.lock();
                                let _ = h.insert_text(ev.event_id, local_id, &text, ev.created_at);
                                h.list("", 100).unwrap_or_default()
                            };
                            {
                                let mut u = ui_c.lock();
                                u.history = preview;
                                u.toast = Some("文字已同步到其他设备".into());
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
                                ui_c.lock().toast =
                                    Some(format!("图片处理失败：{e}"));
                                return;
                            }
                        };
                        let size = png.len() as u64;
                        let max = eng.lock().max_payload_bytes;
                        if size > max {
                            ui_c.lock().toast = Some(format!(
                                "图片过大（约 {} MB），超过本机上限，未同步。可在设置中提高「单次同步上限」。",
                                size / (1024 * 1024)
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
                                u.toast = Some(format!(
                                    "图片已同步到其他设备（{width}×{height}）"
                                ));
                            }
                            hub_c.broadcast_clipboard(ev);
                        }
                    }
                    ClipContent::Files(paths) => {
                        let max = eng.lock().max_payload_bytes;
                        for path in paths {
                            let is_dir = path.is_dir();
                            let (wire_name, bytes, mime) = match inbox::pack_path(&path, max) {
                                Ok(t) => t,
                                Err(e) => {
                                    ui_c.lock().toast =
                                        Some(format!("无法读取文件：{e}"));
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
                            let event_file_name = if is_dir {
                                base_name.clone()
                            } else {
                                wire_name
                            };
                            let ev = {
                                let mut core = eng.lock();
                                core.on_local_file(&event_file_name, bytes, mime)
                            };
                            if let Some(ev) = ev {
                                let list_name = if is_dir {
                                    format!("文件夹 {base_name}")
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
                                        format!("文件夹已同步：{base_name}")
                                    } else {
                                        format!("文件已同步：{base_name}")
                                    });
                                }
                                hub_c.broadcast_clipboard(ev);
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
                            ui_s.lock().toast = Some(format!("保存失败：{e}"));
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
                                            notes.push(
                                                "密码已保存，但当前密码不安全，仍无法配对同步"
                                                    .into(),
                                            );
                                        } else {
                                            notes.push(
                                                "密码已更新，之后新连接将使用新密码".into(),
                                            );
                                        }
                                    }
                                    Err(e) => notes.push(format!("密码更新失败：{e}")),
                                }
                            }
                            if port_changed {
                                notes.push(
                                    "端口已保存，请重启本软件后生效".into(),
                                );
                            }
                            if auto_changed || auto_start {
                                match ohmycopy::autostart::apply(auto_start) {
                                    Ok(()) => notes.push(if auto_start {
                                        "已开启开机自动启动".into()
                                    } else {
                                        "已关闭开机自动启动".into()
                                    }),
                                    Err(e) => notes.push(format!("自动启动设置失败：{e}")),
                                }
                            }
                            if notes.is_empty() {
                                notes.push("设置已保存".into());
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
                            ui_s.lock().toast =
                                Some(format!("已移除设备 {addr}"));
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
                                format!("已暂停与 {addr} 的剪贴板同步")
                            } else {
                                format!("已恢复与 {addr} 的剪贴板同步")
                            });
                        }
                        hub_c.set_ignored(device_id, addr, ignored);
                    }
                    UiCommand::ReloadClients => {
                        match ClientsFile::load_or_create() {
                            Ok(fresh) => {
                                apply_clients(&hub_c, &fresh);
                                let mut cf = clients_c.lock();
                                *cf = fresh;
                                ui_s.lock().saved_clients = cf.clients.clone();
                                ui_s.lock().toast =
                                    Some("设备列表已刷新".into());
                            }
                            Err(e) => {
                                ui_s.lock().toast =
                                    Some(format!("刷新设备列表失败：{e}"));
                            }
                        }
                    }
                    UiCommand::ClearHistory => {
                        let hist_ok = hist.lock().clear().is_ok();
                        let inbox_res = inbox::clear_all();
                        let mut u = ui_s.lock();
                        u.history.clear();
                        u.toast = Some(match (hist_ok, inbox_res) {
                            (true, Ok(())) => "历史记录和接收的临时文件已清空".into(),
                            (true, Err(e)) => {
                                format!("历史已清空，但清理接收文件失败：{e}")
                            }
                            (false, Ok(())) => {
                                "接收文件已清空，但历史记录清理可能失败".into()
                            }
                            (false, Err(e)) => {
                                format!("清空失败：{e}")
                            }
                        });
                    }
                    UiCommand::CopyText(text) => {
                        // History re-copy: file/folder/image rows store local path in content.
                        let path = std::path::PathBuf::from(&text);
                        let result = if path.is_file() {
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
                                            .map(|_| "图片已复制，可以粘贴了".to_string()),
                                        Err(e) => Err(e),
                                    },
                                    Err(e) => Err(anyhow::anyhow!(e)),
                                }
                            } else {
                                clip.set_files_from_sync(&[path])
                                    .map(|_| "已复制到剪贴板".to_string())
                            }
                        } else if path.is_dir() {
                            clip.set_files_from_sync(&[path])
                                .map(|_| "已复制到剪贴板".to_string())
                        } else {
                            clip.set_text_local(&text)
                                .map(|_| "已复制到剪贴板".to_string())
                        };
                        match result {
                            Ok(msg) => ui_s.lock().toast = Some(msg),
                            Err(e) => ui_s.lock().toast = Some(format!("复制失败: {e}")),
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
                eprintln!("图形界面无法启动（本机无可用 OpenGL 2.0+ / wgpu 适配器）。");
                eprintln!("错误: {gui_err}");
                eprintln!("自动切换为【无界面模式】—— 剪贴板同步继续运行。");
                eprintln!("提示: 也可使用参数 --headless 或环境变量 OHMYCOPY_HEADLESS=1 直接进入。");
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
                let _ = ctrlc_wait();
                stop_c.store(true, Ordering::SeqCst);
            })
            .ok();
    }

    println!("====================================================");
    println!(
        " OhMyCopy {}  ·  无界面模式 (headless)",
        env!("CARGO_PKG_VERSION")
    );
    println!("----------------------------------------------------");
    println!(" 设备名称 : {}", cfg.device_name);
    println!(" 设备 ID  : {}", cfg.device_id);
    println!(" 监听端口 : TCP/UDP {}", cfg.tcp_port);
    println!(" 同步开关 : {}", if cfg.sync_enabled { "开" } else { "关" });
    println!(" 工作目录 : {:?}", Config::config_dir().ok());
    println!(" （与 exe 同目录）config.json / clients.json / history.db");
    println!("----------------------------------------------------");
    println!(" 剪贴板同步已在后台运行；请确保各设备共享密码一致。");
    println!(" 可在 clients.json 中设置 auto_connect=true 自动连接。");
    println!(" 有 GUI 的电脑上点「连接」连到本机 IP:{}", cfg.tcp_port);
    println!(" 按 Ctrl+C 退出（或关闭此控制台窗口）");
    println!("====================================================");

    let mut tick = 0u64;
    while !stop.load(Ordering::SeqCst) && !shutdown_flag.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(2));
        tick += 1;
        if tick % 5 == 0 {
            // every ~10s
            let summary = hub.status_summary();
            let n = hub.connected_count();
            println!(
                "[{}] {} (会话 {})",
                chrono::Local::now().format("%H:%M:%S"),
                summary,
                n
            );
        }
    }
    println!("正在退出无界面模式…");
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
        .with_inner_size([720.0, 520.0])
        .with_min_inner_size([480.0, 360.0])
        .with_title(tray::WINDOW_TITLE);
    if let Ok(icon) = ohmycopy::icon::egui_icon_data() {
        vb = vb.with_icon(icon);
    }
    if start_minimized_to_tray {
        // Create window hidden; tray left-click / menu will show it.
        vb = vb.with_visible(false);
    }
    vb
}

/// Pick any usable GPU adapter; prefer discrete/integrated, then CPU/WARP software.
fn select_wgpu_adapter(
    adapters: &[eframe::wgpu::Adapter],
    surface: Option<&eframe::wgpu::Surface<'_>>,
) -> Result<eframe::wgpu::Adapter, String> {
    let compatible = |a: &eframe::wgpu::Adapter| {
        surface
            .map(|s| a.is_surface_supported(s))
            .unwrap_or(true)
    };

    let mut infos: Vec<String> = Vec::new();
    for a in adapters {
        let i = a.get_info();
        infos.push(format!(
            "{} ({:?}/{:?})",
            i.name, i.backend, i.device_type
        ));
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
            if a.get_info().backend == eframe::wgpu::Backend::Dx12
                && is_gpu(a)
                && compatible(a)
            {
                tracing::info!(name = %a.get_info().name, backend = "dx12", "selected GPU adapter");
                return Ok(a.clone());
            }
        }
        for a in adapters {
            if a.get_info().backend == eframe::wgpu::Backend::Dx12
                && is_soft(a)
                && compatible(a)
            {
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
    Err(format!(
        "未找到可用图形适配器。已枚举: {}",
        infos.join(", ")
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
    let make_app = |ui_state: UiState, shared: Arc<Mutex<UiState>>, tx: mpsc::UnboundedSender<UiCommand>| {
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
            Ok(Box::new(AppShell {
                inner: OhMyCopyApp::new(cc, ui_state),
                ui_shared: shared,
                cmd_tx: tx,
                tray,
                true_quit,
                // Force-hide once more on first frame (some backends flash despite with_visible(false)).
                hide_on_start: start_min,
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
                "图形界面启动失败（本机可能无可用 GPU/OpenGL/WARP）。\n\
                 详情: {e}\n\
                 建议: 安装显卡驱动，或确认 Windows 自带 Microsoft Basic Render Driver (WARP) 可用。"
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
    /// One-shot: hide main window right after first frame (start minimized to tray).
    hide_on_start: bool,
}

fn peers_eq(a: &[ohmycopy::net::peer::PeerSnapshot], b: &[ohmycopy::net::peer::PeerSnapshot]) -> bool {
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

fn history_eq(
    a: &[ohmycopy::history::HistoryItem],
    b: &[ohmycopy::history::HistoryItem],
) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| {
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

        // Start minimized to tray: hide once after window exists (covers backends
        // that ignore ViewportBuilder::with_visible(false)).
        if self.hide_on_start {
            self.hide_on_start = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            tray::win32_set_window_visible(tray::WINDOW_TITLE, false);
            tracing::info!("start_minimized_to_tray: main window hidden, tray only");
        }

        // --- Close to tray (X hides; tray "退出" quits) ---
        let want_quit = self.true_quit.load(Ordering::SeqCst)
            || self.tray.as_ref().map(|t| t.is_true_quit()).unwrap_or(false);
        if ctx.input(|i| i.viewport().close_requested()) {
            if !want_quit {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                tray::win32_set_window_visible(tray::WINDOW_TITLE, false);
                if let Some(tray) = &self.tray {
                    tray.set_sync_checked(self.inner.ui.sync_enabled);
                }
            }
        }

        // --- Tray menu / click actions (show/quit also applied in tray callbacks) ---
        if let Some(tray) = &self.tray {
            for action in tray.drain_actions() {
                match action {
                    TrayAction::ShowWindow => {
                        tray::win32_set_window_visible(tray::WINDOW_TITLE, true);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
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

        let sync_before = self.inner.ui.sync_enabled;
        self.inner.update(ctx, frame);

        // Keep tray checkbox aligned with UI toggle.
        if self.inner.ui.sync_enabled != sync_before {
            if let Some(tray) = &self.tray {
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
                    self.inner.ui.toast = Some("已打开数据文件夹".into());
                    self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
                }
                Err(e) => {
                    self.inner.ui.toast = Some(format!("无法打开数据文件夹：{e}"));
                    self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
                }
            }
        }
        if self.inner.ui.cmd_add_manual {
            if let Ok(addr) = self.inner.ui.manual_addr.parse::<SocketAddr>() {
                let _ = self.cmd_tx.send(UiCommand::AddManual(addr));
            } else {
                self.inner.ui.toast =
                    Some("地址格式不对，请填写如 192.168.1.10:3721".into());
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
                    self.inner.ui.toast = Some("设备地址无效，请重试".into());
                    self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
                }
            }
        }
        if let Some((device_id, addr_str)) = self.inner.ui.cmd_remove_client.take() {
            if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                let _ = self.cmd_tx.send(UiCommand::RemoveClient { device_id, addr });
            } else {
                self.inner.ui.toast = Some("设备地址无效".into());
                self.inner.ui.toast_ttl_frames = TOAST_FRAMES;
            }
        }
        if let Some((device_id, addr_str, ignored)) = self.inner.ui.cmd_set_ignore.take() {
            if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                // Keep shared snapshot in sync immediately so the next-frame pull
                // does not revert the optimistic UI flip before the async worker runs.
                if let Some(mut s) = self.ui_shared.try_lock() {
                    for c in &mut s.saved_clients {
                        if (device_id.is_some() && c.device_id == device_id)
                            || c.addr == addr_str
                        {
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
