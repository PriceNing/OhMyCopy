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
    pub status: String,
    pub connected: bool,
    pub connecting: bool,
    pub last_error: Option<String>,
}

impl PeerInfo {
    pub fn snapshot(&self) -> PeerSnapshot {
        PeerSnapshot {
            device_id: self.device_id,
            name: self.name.clone(),
            addr: self.addr.to_string(),
            status: match self.status {
                PeerStatus::Discovered => "已发现".into(),
                PeerStatus::Connecting => "连接中…".into(),
                PeerStatus::Connected => "已连接".into(),
                PeerStatus::AuthFailed => "鉴权失败".into(),
                PeerStatus::Disconnected => "已断开".into(),
            },
            connected: self.status == PeerStatus::Connected,
            connecting: self.status == PeerStatus::Connecting,
            last_error: self.last_error.clone(),
        }
    }
}
