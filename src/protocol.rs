use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 2;
pub const MAGIC: &[u8; 4] = b"OMCP";

/// Numeric tags for documentation / debugging. Wire format uses postcard `Message` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum FrameType {
    Hello = 1,
    AuthChallenge = 2,
    AuthResponse = 3,
    ClipboardEvent = 4,
    Ack = 5,
    Ping = 6,
    Pong = 7,
    Error = 8,
    Unpair = 9,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ContentKind {
    Text,
    Image,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u16,
    pub device_id: Uuid,
    pub device_name: String,
    /// Peer's TCP listen port (for clients.json / reconnect; not the ephemeral source port).
    pub listen_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    pub nonce: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    /// HMAC-like proof: blake3 keyed material over nonce (see auth module).
    pub proof: [u8; 32],
    pub device_id: Uuid,
    pub device_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEvent {
    pub event_id: Uuid,
    pub source_id: Uuid,
    pub content_hash: [u8; 32],
    pub kind: ContentKind,
    pub mime: String,
    pub created_at: u64,
    pub file_name: Option<String>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub code: u16,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    Hello(Hello),
    AuthChallenge(AuthChallenge),
    AuthResponse(AuthResponse),
    ClipboardEvent(ClipboardEvent),
    Ack {
        event_id: Uuid,
    },
    Ping {
        ts: u64,
    },
    Pong {
        ts: u64,
    },
    Error(ErrorMsg),
    /// Intentional unpair: peer should drop session and remove us from clients.json.
    Unpair,
}

impl Message {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// UDP discovery packet (fixed layout, not postcard — keep tiny & language-agnostic).
#[derive(Debug, Clone)]
pub struct DiscoverPacket {
    pub version: u16,
    pub msg_type: u8, // 1=announce 2=goodbye
    pub device_id: Uuid,
    pub tcp_port: u16,
    pub name: String,
    pub caps: u32,
}

pub const CAP_TEXT: u32 = 1;
pub const CAP_IMAGE: u32 = 2;
pub const CAP_FILE: u32 = 4;
pub const DISCOVER_ANNOUNCE: u8 = 1;
pub const DISCOVER_GOODBYE: u8 = 2;

impl DiscoverPacket {
    pub fn encode(&self) -> Vec<u8> {
        let name_bytes = self.name.as_bytes();
        let name_len = name_bytes.len().min(255) as u8;
        let mut buf = Vec::with_capacity(4 + 2 + 1 + 16 + 2 + 1 + name_len as usize + 4);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.push(self.msg_type);
        buf.extend_from_slice(self.device_id.as_bytes());
        buf.extend_from_slice(&self.tcp_port.to_le_bytes());
        buf.push(name_len);
        buf.extend_from_slice(&name_bytes[..name_len as usize]);
        buf.extend_from_slice(&self.caps.to_le_bytes());
        buf
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 4 + 2 + 1 + 16 + 2 + 1 + 4 {
            return None;
        }
        if &data[0..4] != MAGIC {
            return None;
        }
        let version = u16::from_le_bytes([data[4], data[5]]);
        let msg_type = data[6];
        let mut id_bytes = [0u8; 16];
        id_bytes.copy_from_slice(&data[7..23]);
        let device_id = Uuid::from_bytes(id_bytes);
        let tcp_port = u16::from_le_bytes([data[23], data[24]]);
        let name_len = data[25] as usize;
        if data.len() < 26 + name_len + 4 {
            return None;
        }
        let name = String::from_utf8_lossy(&data[26..26 + name_len]).into_owned();
        let caps_off = 26 + name_len;
        let caps = u32::from_le_bytes([
            data[caps_off],
            data[caps_off + 1],
            data[caps_off + 2],
            data[caps_off + 3],
        ]);
        Some(Self {
            version,
            msg_type,
            device_id,
            tcp_port,
            name,
            caps,
        })
    }
}

/// Wire frame: [u32 le length][encrypted or plain body]
pub fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip() {
        let msg = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: Uuid::new_v4(),
            device_name: "A".into(),
            listen_port: 3721,
        });
        let bytes = msg.encode().unwrap();
        let back = Message::decode(&bytes).unwrap();
        match back {
            Message::Hello(h) => {
                assert_eq!(h.device_name, "A");
                assert_eq!(h.listen_port, 3721);
            }
            _ => panic!("wrong type"),
        }
    }

    #[test]
    fn discover_roundtrip() {
        let p = DiscoverPacket {
            version: PROTOCOL_VERSION,
            msg_type: DISCOVER_ANNOUNCE,
            device_id: Uuid::new_v4(),
            tcp_port: 3721,
            name: "客厅".into(),
            caps: CAP_TEXT | CAP_IMAGE,
        };
        let bytes = p.encode();
        let back = DiscoverPacket::decode(&bytes).unwrap();
        assert_eq!(back.tcp_port, 3721);
        assert_eq!(back.name, "客厅");
        assert_eq!(back.device_id, p.device_id);
    }
}
