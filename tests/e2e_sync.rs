//! Protocol-level two-peer auth + clipboard event (no GUI, localhost).

use ohmycopy::auth::{random_nonce, AuthMaterial, SessionCrypto};
use ohmycopy::engine::EngineCore;
use ohmycopy::protocol::{
    encode_frame, AuthChallenge, AuthResponse, ContentKind, Hello, Message, PROTOCOL_VERSION,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

async fn write_frame(w: &mut (impl AsyncWriteExt + Unpin), body: &[u8]) {
    let f = encode_frame(body);
    w.write_all(&f).await.unwrap();
    w.flush().await.unwrap();
}

async fn read_frame(r: &mut (impl AsyncReadExt + Unpin)) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    assert!(len < 64 * 1024 * 1024, "frame too large for unit test");
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.unwrap();
    body
}

#[tokio::test]
async fn two_peers_auth_and_clipboard_event() {
    let password = "test-family-pass";
    let auth = AuthMaterial::from_password(password).unwrap();
    let auth_server = auth.clone();
    let id_a = Uuid::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    let id_b = Uuid::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let auth = auth_server;
        let (stream, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = stream.into_split();

        let hello = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: id_b,
            device_name: "B".into(),
            listen_port: addr.port(),
        });
        write_frame(&mut writer, &hello.encode().unwrap()).await;
        let remote = Message::decode(&read_frame(&mut reader).await).unwrap();
        match remote {
            Message::Hello(h) => assert_eq!(h.device_id, id_a),
            _ => panic!("hello"),
        }

        let server_nonce = random_nonce();
        write_frame(
            &mut writer,
            &Message::AuthChallenge(AuthChallenge {
                nonce: server_nonce,
            })
            .encode()
            .unwrap(),
        )
        .await;
        match Message::decode(&read_frame(&mut reader).await).unwrap() {
            Message::AuthResponse(r) => {
                assert!(auth.verify_proof(&server_nonce, &r.proof));
            }
            _ => panic!("auth resp"),
        }
        let client_nonce = match Message::decode(&read_frame(&mut reader).await).unwrap() {
            Message::AuthChallenge(c) => c.nonce,
            _ => panic!("chal"),
        };
        let proof = auth.prove(&client_nonce);
        write_frame(
            &mut writer,
            &Message::AuthResponse(AuthResponse {
                proof,
                device_id: id_b,
                device_name: "B".into(),
            })
            .encode()
            .unwrap(),
        )
        .await;

        let sk = auth.session_key(&client_nonce, &server_nonce);
        let mut h = blake3::Hasher::new_keyed(&sk);
        h.update(b"send-from-smaller-id");
        let key_from_smaller = *h.finalize().as_bytes();
        let recv = SessionCrypto::new(&key_from_smaller);
        let mut recv_max = 0u64;

        let sealed = read_frame(&mut reader).await;
        let plain = recv.open(&sealed, &mut recv_max).unwrap();
        match Message::decode(&plain).unwrap() {
            Message::ClipboardEvent(ev) => {
                assert_eq!(ev.payload, b"hello from A");
                let mut eng = EngineCore::new(id_b, 1024 * 1024, true);
                assert!(eng.on_remote_event(&ev));
                assert!(!eng.on_remote_event(&ev));
            }
            _ => panic!("expected clipboard"),
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();

    write_frame(
        &mut writer,
        &Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: id_a,
            device_name: "A".into(),
            listen_port: 3721,
        })
        .encode()
        .unwrap(),
    )
    .await;
    match Message::decode(&read_frame(&mut reader).await).unwrap() {
        Message::Hello(h) => assert_eq!(h.device_id, id_b),
        _ => panic!("hello b"),
    }

    let server_nonce = match Message::decode(&read_frame(&mut reader).await).unwrap() {
        Message::AuthChallenge(c) => c.nonce,
        _ => panic!("chal"),
    };
    write_frame(
        &mut writer,
        &Message::AuthResponse(AuthResponse {
            proof: auth.prove(&server_nonce),
            device_id: id_a,
            device_name: "A".into(),
        })
        .encode()
        .unwrap(),
    )
    .await;
    let client_nonce = random_nonce();
    write_frame(
        &mut writer,
        &Message::AuthChallenge(AuthChallenge {
            nonce: client_nonce,
        })
        .encode()
        .unwrap(),
    )
    .await;
    match Message::decode(&read_frame(&mut reader).await).unwrap() {
        Message::AuthResponse(r) => assert!(auth.verify_proof(&client_nonce, &r.proof)),
        _ => panic!("resp"),
    }

    let sk = auth.session_key(&client_nonce, &server_nonce);
    let mut h = blake3::Hasher::new_keyed(&sk);
    h.update(b"send-from-smaller-id");
    let key_from_smaller = *h.finalize().as_bytes();
    let mut send = SessionCrypto::new(&key_from_smaller);
    let mut eng = EngineCore::new(id_a, 1024 * 1024, true);
    let ev = eng.on_local_text("hello from A").unwrap();
    assert_eq!(ev.kind, ContentKind::Text);
    let sealed = send
        .seal(&Message::ClipboardEvent(ev).encode().unwrap())
        .unwrap();
    write_frame(&mut writer, &sealed).await;

    server.await.unwrap();
}

#[test]
fn discover_packet_utf8_name() {
    use ohmycopy::protocol::{DiscoverPacket, CAP_TEXT, DISCOVER_ANNOUNCE, PROTOCOL_VERSION};
    let p = DiscoverPacket {
        version: PROTOCOL_VERSION,
        msg_type: DISCOVER_ANNOUNCE,
        device_id: Uuid::new_v4(),
        tcp_port: 3721,
        name: "书房-PC".into(),
        caps: CAP_TEXT,
    };
    let back = DiscoverPacket::decode(&p.encode()).unwrap();
    assert_eq!(back.name, "书房-PC");
}
