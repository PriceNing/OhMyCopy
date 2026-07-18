use crate::auth::{random_nonce, AuthMaterial, SessionCrypto};
use crate::engine::{now_ms, SharedEngine};
use crate::net::peer::{PeerInfo, PeerSnapshot, PeerStatus};
use crate::protocol::{
    encode_frame, AuthChallenge, AuthResponse, ClipboardEvent, Hello, Message, PROTOCOL_VERSION,
};
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, RwLock};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Handshake / auth / small control frames: fail fast if peer is dead.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
/// Wait for next frame to *start* (idle). Must be longer than heartbeat interval.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
/// Absolute max encrypted frame size (allows config max_payload up to ~480 MiB + AEAD/postcard overhead).
const MAX_FRAME_BYTES: usize = 512 * 1024 * 1024;
/// Pre-auth / handshake frames (Hello, Auth*) must stay small — never allocate up to MAX_FRAME_BYTES.
const MAX_HANDSHAKE_FRAME_BYTES: usize = 64 * 1024;
/// Floor for bulk transfer timeout (large files).
const BULK_IO_MIN: Duration = Duration::from_secs(60);
/// Cap bulk transfer timeout (very large / slow links).
const BULK_IO_MAX: Duration = Duration::from_secs(30 * 60);

/// Timeout scales with payload size so 100–200 MiB on LAN does not hit a fixed 15s wall.
/// Assumes a conservative ~8 MiB/s effective rate after encrypt + TCP.
fn bulk_io_timeout(payload_len: usize) -> Duration {
    let mb = (payload_len as u64).div_ceil(1024 * 1024).max(1);
    // 60s base + 3s per MiB, clamped.
    let secs = 60u64.saturating_add(mb.saturating_mul(3));
    Duration::from_secs(secs).clamp(BULK_IO_MIN, BULK_IO_MAX)
}

#[derive(Clone, Debug)]
pub enum NetEvent {
    PeerUpdated,
    ClipboardFromRemote(ClipboardEvent),
    /// Sticky operational status (listen / summary).
    Status(String),
    /// Short-lived user-visible notice (success / failure).
    Toast(String),
    FirewallHint(String),
    /// Auth + handshake OK — both sides should persist mutual pairing.
    PeerSessionReady {
        device_id: Uuid,
        name: String,
        /// Peer's **listen** address (ip + listen_port), safe for clients.json.
        addr: SocketAddr,
        we_dialed: bool,
    },
    /// Password / auth failed (session not established).
    PeerAuthFailed {
        device_id: Uuid,
        name: String,
        addr: SocketAddr,
    },
    /// Peer sent Unpair (or we finished local unpair after notify).
    PeerUnpaired {
        device_id: Uuid,
        name: String,
        addr: SocketAddr,
        /// true = remote asked us to unpair; false = we initiated.
        from_remote: bool,
    },
}

struct LivePeer {
    addr: SocketAddr,
    tx: mpsc::Sender<Vec<u8>>,
}

pub struct NetworkHub {
    local_id: Uuid,
    local_name: String,
    /// Shared-password material; can be replaced when user saves a new password.
    password_auth: parking_lot::RwLock<AuthMaterial>,
    /// When true, password is the insecure factory default — refuse pair / send.
    insecure_password: std::sync::atomic::AtomicBool,
    engine: SharedEngine,
    peers_meta: Arc<RwLock<HashMap<Uuid, PeerInfo>>>,
    live: Arc<Mutex<HashMap<Uuid, LivePeer>>>,
    /// In-flight dial/handshake (prevents connection storms).
    connecting: Arc<Mutex<HashSet<Uuid>>>,
    connecting_addrs: Arc<Mutex<HashSet<SocketAddr>>>,
    events: broadcast::Sender<NetEvent>,
    known: Arc<Mutex<HashMap<Uuid, SocketAddr>>>,
    /// Peers the user asked to keep connected (auto-reconnect only for these).
    wanted_ids: Arc<Mutex<HashSet<Uuid>>>,
    wanted_addrs: Arc<Mutex<HashSet<SocketAddr>>>,
    /// Muted peers: no clipboard send/receive (connection may stay up).
    ignored_ids: Arc<Mutex<HashSet<Uuid>>>,
    ignored_addrs: Arc<Mutex<HashSet<SocketAddr>>>,
    manual_addrs: Arc<Mutex<Vec<SocketAddr>>>,
    listen_port: u16,
    /// Allows `spawn` from any thread (e.g. main before enter, clipboard watcher).
    rt: tokio::runtime::Handle,
}

impl NetworkHub {
    pub fn new(
        local_id: Uuid,
        local_name: String,
        password: &str,
        engine: SharedEngine,
        listen_port: u16,
        rt: tokio::runtime::Handle,
    ) -> Result<Self> {
        let password_auth = AuthMaterial::from_password(password)?;
        let (events, _) = broadcast::channel(256);
        Ok(Self {
            local_id,
            local_name,
            password_auth: parking_lot::RwLock::new(password_auth),
            insecure_password: std::sync::atomic::AtomicBool::new(
                crate::config::Config::is_insecure_default_password(password),
            ),
            engine,
            peers_meta: Arc::new(RwLock::new(HashMap::new())),
            live: Arc::new(Mutex::new(HashMap::new())),
            connecting: Arc::new(Mutex::new(HashSet::new())),
            connecting_addrs: Arc::new(Mutex::new(HashSet::new())),
            events,
            known: Arc::new(Mutex::new(HashMap::new())),
            wanted_ids: Arc::new(Mutex::new(HashSet::new())),
            wanted_addrs: Arc::new(Mutex::new(HashSet::new())),
            ignored_ids: Arc::new(Mutex::new(HashSet::new())),
            ignored_addrs: Arc::new(Mutex::new(HashSet::new())),
            manual_addrs: Arc::new(Mutex::new(Vec::new())),
            listen_port,
            rt,
        })
    }

    /// Hot-update shared password for future handshakes (ports still need restart).
    pub fn update_password(&self, password: &str) -> Result<()> {
        let auth = AuthMaterial::from_password(password)?;
        *self.password_auth.write() = auth;
        self.insecure_password.store(
            crate::config::Config::is_insecure_default_password(password),
            std::sync::atomic::Ordering::SeqCst,
        );
        Ok(())
    }

    pub fn has_insecure_default_password(&self) -> bool {
        self.insecure_password
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn refuse_if_insecure(&self) -> Result<()> {
        if self.has_insecure_default_password() {
            let _ = self.events.send(NetEvent::Toast(
                "请先在设置里填写自己的共享密码，然后才能连接其他电脑。".into(),
            ));
            bail!("insecure default password");
        }
        Ok(())
    }

    pub fn is_ignored(&self, device_id: Uuid, addr: SocketAddr) -> bool {
        self.ignored_ids.lock().contains(&device_id)
            || self.ignored_addrs.lock().contains(&addr)
    }

    /// Update ignore flag for a client (bidirectional clipboard mute).
    pub fn set_ignored(&self, device_id: Option<Uuid>, addr: SocketAddr, ignored: bool) {
        if ignored {
            self.ignored_addrs.lock().insert(addr);
            if let Some(id) = device_id {
                self.ignored_ids.lock().insert(id);
            }
        } else {
            self.ignored_addrs.lock().remove(&addr);
            if let Some(id) = device_id {
                self.ignored_ids.lock().remove(&id);
            }
        }
    }

    /// Replace ignore sets from clients.json.
    pub fn sync_ignored(&self, ids: impl IntoIterator<Item = Uuid>, addrs: impl IntoIterator<Item = SocketAddr>) {
        *self.ignored_ids.lock() = ids.into_iter().collect();
        *self.ignored_addrs.lock() = addrs.into_iter().collect();
    }

    pub fn connected_count(&self) -> usize {
        self.live.lock().len()
    }

    pub fn status_summary(&self) -> String {
        let n = self.connected_count();
        let connecting = self.connecting.lock().len() + self.connecting_addrs.lock().len();
        if n > 0 {
            format!("已连接 {n} 台设备")
        } else if connecting > 0 {
            "正在连接…".into()
        } else {
            "已就绪，等待连接".into()
        }
    }

    /// Spawn on the hub's runtime — safe outside `tokio::spawn` context.
    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.rt.spawn(fut);
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<NetEvent> {
        self.events.subscribe()
    }

    pub async fn peer_snapshots(&self) -> Vec<PeerSnapshot> {
        let map = self.peers_meta.read().await;
        let mut v: Vec<_> = map.values().map(|p| p.snapshot()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Discovery update only — never dials by itself. Auto-reconnect uses `wanted_*`.
    pub fn note_peer(self: &Arc<Self>, device_id: Uuid, name: String, addr: SocketAddr) {
        if device_id == self.local_id {
            return;
        }
        self.known.lock().insert(device_id, addr);
        let wanted = self.wanted_ids.lock().contains(&device_id);
        let hub = Arc::clone(self);
        self.spawn(async move {
            // Don't downgrade Connected/Connecting to Discovered.
            {
                let map = hub.peers_meta.read().await;
                if let Some(p) = map.get(&device_id) {
                    if p.status == PeerStatus::Connected || p.status == PeerStatus::Connecting {
                        // Still refresh addr/name for reconnect bookkeeping.
                        return;
                    }
                }
            }
            hub.upsert_meta(device_id, name, addr, PeerStatus::Discovered, None)
                .await;
            // Only auto-dial peers already in clients (wanted).
            if wanted && !hub.live.lock().contains_key(&device_id) {
                hub.force_dial(device_id, addr).await;
            }
        });
    }

    /// User clicked 「连接」 on a discovered device — trial dial.
    /// Does **not** mark wanted / persist clients; that happens only after
    /// `PeerSessionReady` (password OK). Auth fail → `PeerAuthFailed`.
    pub fn trial_connect(self: &Arc<Self>, device_id: Uuid, addr: SocketAddr) {
        if device_id == self.local_id {
            return;
        }
        if self.refuse_if_insecure().is_err() {
            return;
        }
        self.known.lock().insert(device_id, addr);
        let _ = self.events.send(NetEvent::Toast(format!("正在连接 {addr} …")));
        let hub = Arc::clone(self);
        self.spawn(async move {
            hub.force_dial(device_id, addr).await;
        });
    }

    /// Manual IP trial — same rules as discovery connect (persist only on success).
    pub fn trial_connect_addr(self: &Arc<Self>, addr: SocketAddr) {
        if self.refuse_if_insecure().is_err() {
            return;
        }
        let _ = self
            .events
            .send(NetEvent::Toast(format!("正在连接 {addr} …")));
        let hub = Arc::clone(self);
        self.spawn(async move {
            hub.dial_addr(addr).await;
        });
    }

    /// Mark peer as a saved client: auto-reconnect target.
    pub fn mark_wanted(&self, device_id: Option<Uuid>, addr: SocketAddr) {
        self.wanted_addrs.lock().insert(addr);
        if let Some(id) = device_id {
            if id != self.local_id {
                self.wanted_ids.lock().insert(id);
                self.known.lock().insert(id, addr);
            }
        }
    }

    /// Ensure wanted + dial if not live (for clients.json entries).
    pub fn ensure_client(self: &Arc<Self>, device_id: Option<Uuid>, addr: SocketAddr) {
        self.mark_wanted(device_id, addr);
        if let Some(id) = device_id {
            if self.live.lock().contains_key(&id) {
                return;
            }
            let hub = Arc::clone(self);
            self.spawn(async move {
                hub.force_dial(id, addr).await;
            });
        } else {
            if self.live.lock().values().any(|p| p.addr == addr) {
                return;
            }
            {
                let mut m = self.manual_addrs.lock();
                if !m.contains(&addr) {
                    m.push(addr);
                }
            }
            let hub = Arc::clone(self);
            self.spawn(async move {
                hub.dial_addr(addr).await;
            });
        }
    }

    /// Remove from clients: optionally notify peer first (mutual unpair), then drop session.
    pub fn remove_client(self: &Arc<Self>, device_id: Option<Uuid>, addr: SocketAddr) {
        self.remove_client_inner(device_id, addr, true);
    }

    /// Drop without notifying (peer already sent Unpair / remote-initiated).
    pub fn remove_client_silent(self: &Arc<Self>, device_id: Option<Uuid>, addr: SocketAddr) {
        self.remove_client_inner(device_id, addr, false);
    }

    fn remove_client_inner(self: &Arc<Self>, device_id: Option<Uuid>, addr: SocketAddr, notify: bool) {
        if notify {
            // Best-effort Unpair so the other side also drops clients.json entry.
            let payload = Message::Unpair.encode().ok();
            if let Some(bytes) = payload {
                let live = self.live.lock();
                if let Some(id) = device_id {
                    if let Some(p) = live.get(&id) {
                        let _ = p.tx.try_send(bytes);
                    }
                } else if let Some((_, p)) = live.iter().find(|(_, p)| p.addr == addr) {
                    let _ = p.tx.try_send(bytes);
                }
            }
        }

        let hub = Arc::clone(self);
        self.spawn(async move {
            // Give the Unpair frame a moment to flush.
            if notify {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            hub.wanted_addrs.lock().remove(&addr);
            hub.manual_addrs.lock().retain(|a| *a != addr);
            hub.ignored_addrs.lock().remove(&addr);
            if let Some(id) = device_id {
                hub.wanted_ids.lock().remove(&id);
                hub.ignored_ids.lock().remove(&id);
                hub.live.lock().remove(&id);
                hub.upsert_meta(
                    id,
                    id.to_string(),
                    addr,
                    PeerStatus::Disconnected,
                    Some("已解除配对".into()),
                )
                .await;
            } else {
                let ids: Vec<Uuid> = {
                    let mut live = hub.live.lock();
                    let ids: Vec<Uuid> = live
                        .iter()
                        .filter(|(_, p)| p.addr == addr)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in &ids {
                        live.remove(id);
                    }
                    ids
                };
                for id in ids {
                    hub.upsert_meta(
                        id,
                        id.to_string(),
                        addr,
                        PeerStatus::Disconnected,
                        Some("已解除配对".into()),
                    )
                    .await;
                }
            }
            let _ = hub.events.send(NetEvent::Status(hub.status_summary()));
        });
    }

    /// Replace wanted set from clients.json (hot-reload / full resync).
    pub fn sync_from_clients(self: &Arc<Self>, entries: &[(Option<Uuid>, SocketAddr)]) {
        let mut new_ids = HashSet::new();
        let mut new_addrs = HashSet::new();
        for (id, addr) in entries {
            new_addrs.insert(*addr);
            if let Some(id) = id {
                if *id != self.local_id {
                    new_ids.insert(*id);
                    self.known.lock().insert(*id, *addr);
                }
            }
        }
        // Drop sessions no longer in clients.
        {
            let mut live = self.live.lock();
            let drop_ids: Vec<Uuid> = live
                .keys()
                .copied()
                .filter(|id| !new_ids.contains(id))
                .collect();
            // Also drop manual-only by addr if addr not wanted
            let drop_by_addr: Vec<Uuid> = live
                .iter()
                .filter(|(id, p)| !new_ids.contains(id) && !new_addrs.contains(&p.addr))
                .map(|(id, _)| *id)
                .collect();
            for id in drop_ids.into_iter().chain(drop_by_addr) {
                live.remove(&id);
            }
        }
        *self.wanted_ids.lock() = new_ids;
        *self.wanted_addrs.lock() = new_addrs.clone();
        // Dial each client not live.
        for (id, addr) in entries {
            self.ensure_client(*id, *addr);
        }
    }

    /// Send a local clipboard event to all connected peers.
    pub fn broadcast_clipboard(&self, ev: ClipboardEvent) {
        if self.refuse_if_insecure().is_err() {
            return;
        }
        self.relay_clipboard(&ev, None);
    }

    /// Relay an event to connected peers, optionally skipping one peer
    /// (the hop we just received from). Enables A↔B↔C star/mesh topology:
    /// B copies → A applies + relays to C (and not back to B).
    /// Ignored clients never receive clipboard frames.
    pub fn relay_clipboard(&self, ev: &ClipboardEvent, except_peer: Option<Uuid>) {
        let body = match Message::ClipboardEvent(ev.clone()).encode() {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(error = %e, "encode clipboard event");
                tracing::error!(error = %e, "encode clipboard event");
                let _ = self.events.send(NetEvent::Toast(
                    "同步失败，内容无法发送。".into(),
                ));
                return;
            }
        };
        let body_len = body.len();
        if body_len > MAX_FRAME_BYTES {
            tracing::error!(body_len, max = MAX_FRAME_BYTES, "clipboard event exceeds frame cap");
            let _ = self.events.send(NetEvent::Toast(
                "内容过大，无法同步。请在设置中提高「单次同步上限」，或拆分文件。".into(),
            ));
            return;
        }
        let ignored_ids = self.ignored_ids.lock().clone();
        let ignored_addrs = self.ignored_addrs.lock().clone();
        let targets: Vec<(Uuid, mpsc::Sender<Vec<u8>>)> = {
            let live = self.live.lock();
            live.iter()
                .filter(|(peer_id, peer)| {
                    Some(**peer_id) != except_peer
                        && !ignored_ids.contains(peer_id)
                        && !ignored_addrs.contains(&peer.addr)
                })
                .map(|(id, p)| (*id, p.tx.clone()))
                .collect()
        };
        let mut sent = 0usize;
        let mut failed = 0usize;
        for (_peer_id, tx) in targets {
            // Large payloads: never drop with try_send when the queue is briefly full —
            // queue an async send so 100MB+ transfers still go out.
            match tx.try_send(body.clone()) {
                Ok(()) => sent += 1,
                Err(tokio::sync::mpsc::error::TrySendError::Full(b)) => {
                    let tx2 = tx.clone();
                    self.rt.spawn(async move {
                        if tx2.send(b).await.is_err() {
                            tracing::warn!("peer channel closed while sending large clipboard");
                        }
                    });
                    sent += 1;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    failed += 1;
                }
            }
        }
        if sent > 0 {
            tracing::info!(
                event = %ev.event_id,
                except = ?except_peer,
                sent,
                body_len,
                "relayed clipboard event"
            );
        } else if failed > 0 || except_peer.is_none() {
            tracing::warn!(failed, body_len, "clipboard event not delivered to any peer");
            let _ = self.events.send(NetEvent::Toast(
                "当前没有已连接的设备，内容未同步出去。".into(),
            ));
        }
    }

    pub async fn run(self: Arc<Self>, listen_addr: SocketAddr, mut shutdown: mpsc::Receiver<()>) {
        let listener = match TcpListener::bind(listen_addr).await {
            Ok(l) => {
                tracing::info!(%listen_addr, "TCP listening");
                let _ = self.events.send(NetEvent::Status("已就绪，等待连接".into()));
                l
            }
            Err(e) => {
                tracing::error!(error = %e, "TCP bind failed");
                let _ = self.events.send(NetEvent::FirewallHint(format!(
                    "无法使用端口 {}（可能被占用或被防火墙拦截）。请换一个端口，或在系统防火墙中允许 OhMyCopy。",
                    listen_addr.port()
                )));
                let _ = shutdown.recv().await;
                return;
            }
        };

        let reconnect = {
            let hub = Arc::clone(&self);
            self.rt.spawn(async move {
                loop {
                    let base = Duration::from_secs(4);
                    let jitter = Duration::from_millis(rand::random::<u64>() % 2000);
                    tokio::time::sleep(base + jitter).await;

                    // Only auto-reconnect peers the user previously chose to connect.
                    let wanted: Vec<(Uuid, SocketAddr)> = {
                        let wanted_ids = hub.wanted_ids.lock().clone();
                        let known = hub.known.lock();
                        known
                            .iter()
                            .filter(|(id, _)| wanted_ids.contains(id))
                            .map(|(k, v)| (*k, *v))
                            .collect()
                    };
                    for (id, addr) in wanted {
                        if hub.live.lock().contains_key(&id) {
                            continue;
                        }
                        if hub.connecting.lock().contains(&id) {
                            continue;
                        }
                        let h = Arc::clone(&hub);
                        hub.rt.spawn(async move {
                            h.force_dial(id, addr).await;
                        });
                    }

                    let manuals: Vec<SocketAddr> = {
                        let wanted_addrs = hub.wanted_addrs.lock().clone();
                        let manuals = hub.manual_addrs.lock().clone();
                        manuals
                            .into_iter()
                            .filter(|a| wanted_addrs.contains(a))
                            .collect()
                    };
                    for addr in manuals {
                        if hub.live.lock().values().any(|p| p.addr == addr) {
                            continue;
                        }
                        if hub.connecting_addrs.lock().contains(&addr) {
                            continue;
                        }
                        let h = Arc::clone(&hub);
                        hub.rt.spawn(async move {
                            h.dial_addr(addr).await;
                        });
                    }
                }
            })
        };

        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    reconnect.abort();
                    break;
                }
                acc = listener.accept() => {
                    match acc {
                        Ok((stream, addr)) => {
                            let hub = Arc::clone(&self);
                            self.rt.spawn(async move {
                                if let Err(e) = hub.handle_connection(stream, addr, false).await {
                                    tracing::debug!(%addr, error = %e, "inbound ended");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "accept error");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        }
    }

    async fn upsert_meta(
        &self,
        device_id: Uuid,
        name: String,
        addr: SocketAddr,
        status: PeerStatus,
        err: Option<String>,
    ) {
        {
            let mut map = self.peers_meta.write().await;
            map.insert(
                device_id,
                PeerInfo {
                    device_id,
                    name,
                    addr,
                    status,
                    last_seen: Instant::now(),
                    last_error: err,
                },
            );
        }
        let _ = self.events.send(NetEvent::PeerUpdated);
    }

    /// Explicit connect / auto-reconnect: either side may dial.
    async fn force_dial(self: &Arc<Self>, remote_id: Uuid, addr: SocketAddr) {
        if remote_id == self.local_id {
            return;
        }
        if self.live.lock().contains_key(&remote_id) {
            // Already live — still ensure pairing is recorded for the dialer.
            let name = {
                let map = self.peers_meta.read().await;
                map.get(&remote_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| remote_id.to_string())
            };
            let _ = self.events.send(NetEvent::PeerSessionReady {
                device_id: remote_id,
                name,
                addr,
                we_dialed: true,
            });
            return;
        }
        if !self.connecting.lock().insert(remote_id) {
            return;
        }
        let result = self.dial_addr_as(remote_id, addr).await;
        self.connecting.lock().remove(&remote_id);
        if let Err(e) = result {
            tracing::debug!(%remote_id, %addr, error = %e, "force_dial finished with error");
            let msg = e.to_string();
            // Auth failures already emitted PeerAuthFailed + toast via that path.
            if !msg.contains("auth") {
                let _ = self.events.send(NetEvent::Toast(format!(
                    "无法连接 {addr}，请确认对方已打开软件且网络畅通"
                )));
            }
            let _ = self
                .events
                .send(NetEvent::Status(self.status_summary()));
        }
    }

    async fn dial_addr(self: &Arc<Self>, addr: SocketAddr) {
        if self.live.lock().values().any(|p| p.addr == addr) {
            return;
        }
        if !self.connecting_addrs.lock().insert(addr) {
            return;
        }
        let outcome = async {
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
                .await
                .map_err(|_| anyhow::anyhow!("connect timeout"))?
                .map_err(|e| anyhow::anyhow!("connect: {e}"))?;
            let hub = Arc::clone(self);
            // Run handshake in this task so connecting_addrs covers full attempt.
            hub.handle_connection(stream, addr, true).await
        }
        .await;
        self.connecting_addrs.lock().remove(&addr);
        if let Err(e) = outcome {
            tracing::debug!(%addr, error = %e, "manual/unknown dial ended");
        }
    }

    async fn dial_addr_as(self: &Arc<Self>, remote_id: Uuid, addr: SocketAddr) -> Result<()> {
        self.upsert_meta(
            remote_id,
            remote_id.to_string(),
            addr,
            PeerStatus::Connecting,
            None,
        )
        .await;

        let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                self.upsert_meta(
                    remote_id,
                    remote_id.to_string(),
                    addr,
                    PeerStatus::Disconnected,
                    Some(e.to_string()),
                )
                .await;
                bail!("connect: {e}");
            }
            Err(_) => {
                self.upsert_meta(
                    remote_id,
                    remote_id.to_string(),
                    addr,
                    PeerStatus::Disconnected,
                    Some("连接超时".into()),
                )
                .await;
                bail!("connect timeout");
            }
        };

        if let Err(e) = self
            .clone()
            .handle_connection(stream, addr, true)
            .await
        {
            self.upsert_meta(
                remote_id,
                remote_id.to_string(),
                addr,
                PeerStatus::Disconnected,
                Some(e.to_string()),
            )
            .await;
            return Err(e);
        }
        Ok(())
    }

    async fn handle_connection(
        self: Arc<Self>,
        stream: TcpStream,
        conn_addr: SocketAddr,
        we_dialed: bool,
    ) -> Result<()> {
        // Insecure default password: refuse both dial and accept so two fresh installs cannot pair.
        self.refuse_if_insecure()?;

        stream.set_nodelay(true)?;
        let (mut reader, mut writer) = stream.into_split();

        let hello = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: self.local_id,
            device_name: self.local_name.clone(),
            listen_port: self.listen_port,
        });
        write_raw_timeout(&mut writer, &hello.encode()?, HANDSHAKE_TIMEOUT).await?;

        // Hello / Auth are unauthenticated — hard-cap frame size (DoS).
        let remote_hello = read_message_timeout(&mut reader, HANDSHAKE_TIMEOUT).await?;
        let (remote_id, remote_name, remote_listen_port) = match remote_hello {
            Message::Hello(h) => {
                if h.protocol_version != PROTOCOL_VERSION {
                    bail!(
                        "协议版本不匹配（本机 v{} / 对端 v{}），请两端升级到同一版本",
                        PROTOCOL_VERSION,
                        h.protocol_version
                    );
                }
                (h.device_id, h.device_name, h.listen_port)
            }
            _ => bail!("expected Hello"),
        };

        if remote_id == self.local_id {
            bail!("connected to self");
        }

        // Listen address for clients.json (never use inbound ephemeral source port).
        let peer_listen_addr = if we_dialed {
            conn_addr
        } else {
            let known = self.known.lock().get(&remote_id).copied();
            known.unwrap_or_else(|| {
                let port = if remote_listen_port != 0 {
                    remote_listen_port
                } else {
                    self.listen_port
                };
                SocketAddr::new(conn_addr.ip(), port)
            })
        };

        // Single session per peer: first finished handshake wins.
        if self.live.lock().contains_key(&remote_id) {
            tracing::debug!(%remote_id, we_dialed, "already connected — drop extra socket");
            // Ensure pairing still recorded if a parallel dial raced.
            let _ = self.events.send(NetEvent::PeerSessionReady {
                device_id: remote_id,
                name: remote_name,
                addr: peer_listen_addr,
                we_dialed,
            });
            bail!("already connected");
        }

        self.known.lock().insert(remote_id, peer_listen_addr);
        self.manual_addrs
            .lock()
            .retain(|a| *a != peer_listen_addr && *a != conn_addr);

        let session_base = self
            .authenticate(
                &mut reader,
                &mut writer,
                we_dialed,
                remote_id,
                &remote_name,
                peer_listen_addr,
            )
            .await?;

        // Directional AEAD keys: smaller id uses key0 to send; larger uses key1.
        let (key_from_smaller, key_from_larger) = (
            {
                let mut h = blake3::Hasher::new_keyed(&session_base);
                h.update(b"send-from-smaller-id");
                *h.finalize().as_bytes()
            },
            {
                let mut h = blake3::Hasher::new_keyed(&session_base);
                h.update(b"send-from-larger-id");
                *h.finalize().as_bytes()
            },
        );
        let we_are_smaller = self.local_id < remote_id;
        let (send_key, recv_key) = if we_are_smaller {
            (key_from_smaller, key_from_larger)
        } else {
            (key_from_larger, key_from_smaller)
        };

        let mut send_crypto = SessionCrypto::new(&send_key);
        let recv_crypto = SessionCrypto::new(&recv_key);
        let mut recv_max = 0u64;

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
        {
            let mut live = self.live.lock();
            if live.contains_key(&remote_id) {
                bail!("already connected (race)");
            }
            live.insert(
                remote_id,
                LivePeer {
                    addr: peer_listen_addr,
                    tx: tx.clone(),
                },
            );
        }

        self.upsert_meta(
            remote_id,
            remote_name.clone(),
            peer_listen_addr,
            PeerStatus::Connected,
            None,
        )
        .await;
        let _ = self.events.send(NetEvent::PeerSessionReady {
            device_id: remote_id,
            name: remote_name.clone(),
            addr: peer_listen_addr,
            we_dialed,
        });
        let _ = self.events.send(NetEvent::Toast(format!(
            "已与「{remote_name}」连接成功"
        )));
        let _ = self
            .events
            .send(NetEvent::Status(self.status_summary()));

        // For disconnect / ignore checks use listen addr.
        let addr = peer_listen_addr;

        let write_task = self.rt.spawn(async move {
            while let Some(plain) = rx.recv().await {
                let plain_len = plain.len();
                let sealed = match send_crypto.seal(&plain) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(error = %e, plain_len, "seal failed");
                        break;
                    }
                };
                let t = bulk_io_timeout(sealed.len());
                if let Err(e) = write_raw_timeout(&mut writer, &sealed, t).await {
                    tracing::warn!(
                        error = %e,
                        sealed_len = sealed.len(),
                        timeout_secs = t.as_secs(),
                        "write frame failed"
                    );
                    break;
                }
            }
        });

        let ping_tx = tx.clone();
        let heartbeat = self.rt.spawn(async move {
            let mut iv = tokio::time::interval(HEARTBEAT_INTERVAL);
            // Don't fire immediately — handshake just finished; avoid pile-up.
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            iv.tick().await;
            loop {
                let msg = Message::Ping { ts: now_ms() };
                if let Ok(b) = msg.encode() {
                    if ping_tx.send(b).await.is_err() {
                        break;
                    }
                }
                iv.tick().await;
            }
        });

        let read_result = async {
            loop {
                // Wait for next frame header with idle timeout; body uses size-based timeout.
                let sealed =
                    read_raw_timeout(&mut reader, SESSION_IDLE_TIMEOUT, bulk_io_timeout).await?;
                let plain = recv_crypto.open(&sealed, &mut recv_max)?;
                let msg = Message::decode(&plain).context("decode")?;
                match msg {
                    Message::ClipboardEvent(ev) => {
                        // Ignored clients: drop both apply and relay (no clipboard exchange).
                        if self.is_ignored(remote_id, addr) {
                            tracing::debug!(
                                %remote_id,
                                event = %ev.event_id,
                                "drop clipboard from ignored peer"
                            );
                            continue;
                        }
                        // First-seen only (event_id). Then apply locally AND relay
                        // to other peers so star topology works (B→A→C).
                        let apply = {
                            let mut eng = self.engine.lock();
                            eng.on_remote_event(&ev)
                        };
                        if apply {
                            self.relay_clipboard(&ev, Some(remote_id));
                            let _ = self.events.send(NetEvent::ClipboardFromRemote(ev));
                        }
                    }
                    Message::Ping { ts } => {
                        let msg = Message::Pong { ts };
                        if let Ok(b) = msg.encode() {
                            let _ = tx.try_send(b);
                        }
                    }
                    Message::Pong { .. } | Message::Ack { .. } => {}
                    Message::Unpair => {
                        tracing::info!(%remote_id, "received Unpair from peer");
                        let _ = self.events.send(NetEvent::PeerUnpaired {
                            device_id: remote_id,
                            name: remote_name.clone(),
                            addr,
                            from_remote: true,
                        });
                        // Exit session cleanly (do not auto-reconnect as wanted will be cleared).
                        bail!("peer unpaired");
                    }
                    Message::Error(e) => {
                        tracing::warn!(code = e.code, msg = %e.message, "peer error frame");
                    }
                    _ => {}
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        }
        .await;

        heartbeat.abort();
        write_task.abort();
        self.live.lock().remove(&remote_id);
        self.connecting.lock().remove(&remote_id);
        self.connecting_addrs.lock().remove(&conn_addr);
        self.connecting_addrs.lock().remove(&addr);
        let unpair = read_result
            .as_ref()
            .err()
            .map(|e| e.to_string().contains("unpair"))
            .unwrap_or(false);
        self.upsert_meta(
            remote_id,
            remote_name.clone(),
            addr,
            PeerStatus::Disconnected,
            if unpair {
                Some("对端已解除配对".into())
            } else {
                read_result.as_ref().err().map(|e| e.to_string())
            },
        )
        .await;
        if !unpair {
            let _ = self.events.send(NetEvent::Toast(format!(
                "与 {} 的连接已断开",
                remote_name
            )));
        }
        let _ = self
            .events
            .send(NetEvent::Status(self.status_summary()));
        read_result
    }

    async fn authenticate<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        we_dialed: bool,
        remote_id: Uuid,
        remote_name: &str,
        addr: SocketAddr,
    ) -> Result<[u8; 32]>
    where
        R: AsyncReadExt + Unpin,
        W: AsyncWriteExt + Unpin,
    {
        // Snapshot auth material (may hot-update mid-process).
        let auth = self.password_auth.read().clone();

        if !we_dialed {
            let server_nonce = random_nonce();
            write_raw_timeout(
                writer,
                &Message::AuthChallenge(AuthChallenge {
                    nonce: server_nonce,
                })
                .encode()?,
                HANDSHAKE_TIMEOUT,
            )
            .await?;

            match read_message_timeout(reader, HANDSHAKE_TIMEOUT).await? {
                Message::AuthResponse(r) => {
                    check_auth_identity(&r, remote_id, remote_name)?;
                    if !auth.verify_proof(&server_nonce, &r.proof) {
                        self.emit_auth_failed(remote_id, remote_name, addr, "密码不匹配")
                            .await;
                        bail!("auth failed");
                    }
                }
                _ => bail!("expected AuthResponse"),
            }

            let client_nonce = match read_message_timeout(reader, HANDSHAKE_TIMEOUT).await? {
                Message::AuthChallenge(c) => c.nonce,
                _ => bail!("expected reverse AuthChallenge"),
            };
            let proof = auth.prove(&client_nonce);
            write_raw_timeout(
                writer,
                &Message::AuthResponse(AuthResponse {
                    proof,
                    device_id: self.local_id,
                    device_name: self.local_name.clone(),
                })
                .encode()?,
                HANDSHAKE_TIMEOUT,
            )
            .await?;

            Ok(auth.session_key(&client_nonce, &server_nonce))
        } else {
            let server_nonce = match read_message_timeout(reader, HANDSHAKE_TIMEOUT).await? {
                Message::AuthChallenge(c) => c.nonce,
                _ => bail!("expected AuthChallenge"),
            };
            let proof = auth.prove(&server_nonce);
            write_raw_timeout(
                writer,
                &Message::AuthResponse(AuthResponse {
                    proof,
                    device_id: self.local_id,
                    device_name: self.local_name.clone(),
                })
                .encode()?,
                HANDSHAKE_TIMEOUT,
            )
            .await?;

            let client_nonce = random_nonce();
            write_raw_timeout(
                writer,
                &Message::AuthChallenge(AuthChallenge {
                    nonce: client_nonce,
                })
                .encode()?,
                HANDSHAKE_TIMEOUT,
            )
            .await?;

            match read_message_timeout(reader, HANDSHAKE_TIMEOUT).await? {
                Message::AuthResponse(r) => {
                    check_auth_identity(&r, remote_id, remote_name)?;
                    if !auth.verify_proof(&client_nonce, &r.proof) {
                        self.emit_auth_failed(remote_id, remote_name, addr, "密码不匹配")
                            .await;
                        bail!("auth reverse failed");
                    }
                }
                _ => bail!("expected reverse AuthResponse"),
            }

            Ok(auth.session_key(&client_nonce, &server_nonce))
        }
    }

    async fn emit_auth_failed(
        &self,
        remote_id: Uuid,
        remote_name: &str,
        addr: SocketAddr,
        msg: &str,
    ) {
        self.upsert_meta(
            remote_id,
            remote_name.to_string(),
            addr,
            PeerStatus::AuthFailed,
            Some(msg.into()),
        )
        .await;
        let _ = self.events.send(NetEvent::PeerAuthFailed {
            device_id: remote_id,
            name: remote_name.to_string(),
            addr,
        });
    }
}

/// AuthResponse must match the identity claimed in plaintext Hello.
fn check_auth_identity(r: &AuthResponse, remote_id: Uuid, remote_name: &str) -> Result<()> {
    if r.device_id != remote_id {
        bail!(
            "AuthResponse device_id 与 Hello 不一致（{} != {}）",
            r.device_id,
            remote_id
        );
    }
    if r.device_name != remote_name {
        bail!(
            "AuthResponse device_name 与 Hello 不一致（{:?} != {:?}）",
            r.device_name,
            remote_name
        );
    }
    Ok(())
}

async fn write_raw_timeout<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    body: &[u8],
    timeout: Duration,
) -> Result<()> {
    let frame = encode_frame(body);
    // Chunked write so progress keeps flowing on large frames (OS may stall huge write_all).
    tokio::time::timeout(timeout, async {
        const CHUNK: usize = 256 * 1024;
        let mut off = 0;
        while off < frame.len() {
            let end = (off + CHUNK).min(frame.len());
            w.write_all(&frame[off..end]).await?;
            off = end;
        }
        w.flush().await?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("write timeout ({} bytes, {:?})", body.len(), timeout))?
    .context("write")?;
    Ok(())
}

/// Read one length-prefixed frame.
/// `idle_or_total`: used while waiting for the 4-byte length (and as floor for small frames).
/// `body_timeout_for`: once length is known, bulk body uses a size-based timeout.
/// `max_frame`: hard cap before allocating the body buffer (handshake vs session).
async fn read_raw_timeout_max<R: AsyncReadExt + Unpin>(
    r: &mut R,
    idle_or_total: Duration,
    body_timeout_for: impl Fn(usize) -> Duration,
    max_frame: usize,
) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    tokio::time::timeout(idle_or_total, r.read_exact(&mut len_buf))
        .await
        .map_err(|_| anyhow::anyhow!("read timeout (waiting for frame header)"))?
        .context("read frame length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > max_frame {
        return Err(anyhow::anyhow!(
            "frame too large: {len} > max {max_frame}"
        ));
    }
    if len == 0 {
        return Ok(Vec::new());
    }
    let body_timeout = body_timeout_for(len).max(idle_or_total);
    let mut body = vec![0u8; len];
    // Chunked read for large bodies.
    tokio::time::timeout(body_timeout, async {
        const CHUNK: usize = 256 * 1024;
        let mut off = 0;
        while off < len {
            let end = (off + CHUNK).min(len);
            r.read_exact(&mut body[off..end]).await?;
            off = end;
        }
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!("read timeout (body {len} bytes, {:?})", body_timeout)
    })?
    .context("read frame body")?;
    Ok(body)
}

async fn read_raw_timeout<R: AsyncReadExt + Unpin>(
    r: &mut R,
    idle_or_total: Duration,
    body_timeout_for: impl Fn(usize) -> Duration,
) -> Result<Vec<u8>> {
    read_raw_timeout_max(r, idle_or_total, body_timeout_for, MAX_FRAME_BYTES).await
}

async fn read_message_timeout<R: AsyncReadExt + Unpin>(
    r: &mut R,
    timeout: Duration,
) -> Result<Message> {
    // Handshake messages are small — never allocate multi-hundred-MiB buffers.
    let body =
        read_raw_timeout_max(r, timeout, |_| timeout, MAX_HANDSHAKE_FRAME_BYTES).await?;
    Ok(Message::decode(&body)?)
}
