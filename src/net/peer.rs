use std::net::SocketAddr;
use std::time::Instant;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerStatus {
    Discovered,
    Connecting,
    Connected,
    AuthFailed,
    Disconnected,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub device_id: Uuid,
    pub name: String,
    pub addr: SocketAddr,
    pub status: PeerStatus,
    pub last_seen: Instant,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PeerSnapshot {
    pub device_id: Uuid,
    pub name: String,
    pub addr: String,
    /// Localized display label (via i18n at snapshot time).
    pub status: String,
    /// Machine-readable status for UI logic (language-independent).
    pub status_kind: PeerStatus,
    pub connected: bool,
    pub connecting: bool,
    pub last_error: Option<String>,
}

impl PeerStatus {
    pub fn i18n_key(self) -> &'static str {
        match self {
            PeerStatus::Discovered => "peer.discovered",
            PeerStatus::Connecting => "peer.connecting",
            PeerStatus::Connected => "peer.connected",
            PeerStatus::AuthFailed => "peer.auth_failed",
            PeerStatus::Disconnected => "peer.disconnected",
        }
    }
}

impl PeerInfo {
    pub fn snapshot(&self) -> PeerSnapshot {
        PeerSnapshot {
            device_id: self.device_id,
            name: self.name.clone(),
            addr: self.addr.to_string(),
            status: crate::i18n::t(self.status.i18n_key()),
            status_kind: self.status,
            connected: self.status == PeerStatus::Connected,
            connecting: self.status == PeerStatus::Connecting,
            last_error: self.last_error.clone(),
        }
    }
}
