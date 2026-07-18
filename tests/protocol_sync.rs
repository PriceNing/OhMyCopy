//! Lightweight protocol helpers (see also e2e_sync).

use ohmycopy::protocol::{
    ClipboardEvent, ContentKind, DiscoverPacket, Hello, Message, PROTOCOL_VERSION, CAP_TEXT,
    CAP_IMAGE, CAP_FILE, DISCOVER_ANNOUNCE,
};
use uuid::Uuid;

#[test]
fn postcard_message_roundtrip() {
    let msg = Message::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: Uuid::new_v4(),
        device_name: "node".into(),
        listen_port: 3721,
    });
    let bytes = msg.encode().unwrap();
    let back = Message::decode(&bytes).unwrap();
    match back {
        Message::Hello(h) => assert_eq!(h.device_name, "node"),
        _ => panic!("type"),
    }
}

#[test]
fn postcard_unpair_roundtrip() {
    let bytes = Message::Unpair.encode().unwrap();
    let back = Message::decode(&bytes).unwrap();
    assert!(matches!(back, Message::Unpair));
}

#[test]
fn postcard_clipboard_event_text_roundtrip() {
    let text = "中文+emoji 📎";
    let mut hash = [0u8; 32];
    hash[0] = 0xAB;
    let ev = ClipboardEvent {
        event_id: Uuid::new_v4(),
        source_id: Uuid::new_v4(),
        content_hash: hash,
        kind: ContentKind::Text,
        mime: "text/plain".into(),
        created_at: 1_700_000_000_000,
        file_name: None,
        payload: text.as_bytes().to_vec(),
    };
    let msg = Message::ClipboardEvent(ev.clone());
    let bytes = msg.encode().unwrap();
    let back = Message::decode(&bytes).unwrap();
    match back {
        Message::ClipboardEvent(got) => {
            assert_eq!(got.event_id, ev.event_id);
            assert_eq!(got.kind, ContentKind::Text);
            assert_eq!(got.payload, ev.payload);
            assert_eq!(String::from_utf8(got.payload).unwrap(), text);
        }
        _ => panic!("expected ClipboardEvent"),
    }
}

#[test]
fn discover_packet_encode_decode() {
    let id = Uuid::new_v4();
    let pkt = DiscoverPacket {
        version: PROTOCOL_VERSION,
        msg_type: DISCOVER_ANNOUNCE,
        device_id: id,
        tcp_port: 3721,
        name: "E2E-Host".into(),
        caps: CAP_TEXT | CAP_IMAGE | CAP_FILE,
    };
    let bytes = pkt.encode();
    let back = DiscoverPacket::decode(&bytes).expect("decode");
    assert_eq!(back.device_id, id);
    assert_eq!(back.tcp_port, 3721);
    assert_eq!(back.name, "E2E-Host");
    assert_eq!(back.caps, CAP_TEXT | CAP_IMAGE | CAP_FILE);
}
