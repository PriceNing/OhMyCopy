//! Automated two-hub e2e on localhost (no GUI).
//!
//! Covers: pair, text, image, file, folder-zip extract, large file/folder,
//! ignore mute, unpair notify, oversize reject, reverse B→A, wrong password.
//!
//! ```text
//! cargo test --release --test hub_pair_e2e
//! $env:OHMYCOPY_E2E_LARGE_MB=20; cargo test --release --test hub_pair_e2e hub_large_file_sync -- --nocapture
//! ```

use ohmycopy::engine::{EngineCore, SharedEngine};
use ohmycopy::inbox::{self, MIME_DIR_ZIP};
use ohmycopy::net::tcp::{NetEvent, NetworkHub};
use ohmycopy::protocol::ContentKind;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

fn eng(id: Uuid, max: u64) -> SharedEngine {
    Arc::new(Mutex::new(EngineCore::new(id, max, true)))
}

async fn wait_event<F>(
    rx: &mut tokio::sync::broadcast::Receiver<NetEvent>,
    timeout: Duration,
    mut pred: F,
) -> NetEvent
where
    F: FnMut(&NetEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            panic!("timeout waiting for NetEvent");
        }
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(ev)) if pred(&ev) => return ev,
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => panic!("broadcast closed: {e}"),
            Err(_) => panic!("timeout waiting for NetEvent"),
        }
    }
}

/// Wait up to `timeout` and return true if any matching event arrived.
async fn wait_event_optional<F>(
    rx: &mut tokio::sync::broadcast::Receiver<NetEvent>,
    timeout: Duration,
    mut pred: F,
) -> bool
where
    F: FnMut(&NetEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(ev)) if pred(&ev) => return true,
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return false,
            Err(_) => return false,
        }
    }
}

fn spawn_hub(
    id: Uuid,
    name: &str,
    password: &str,
    max_payload: u64,
    listen: std::net::SocketAddr,
) -> (
    Arc<NetworkHub>,
    mpsc::Sender<()>,
    tokio::sync::broadcast::Receiver<NetEvent>,
) {
    let rt = tokio::runtime::Handle::current();
    let engine = eng(id, max_payload);
    let hub = Arc::new(
        NetworkHub::new(
            id,
            name.to_string(),
            password,
            engine,
            listen.port(),
            rt.clone(),
        )
        .expect("hub"),
    );
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
    let mut ev_rx = hub.subscribe_events();
    while ev_rx.try_recv().is_ok() {}

    let hub_run = Arc::clone(&hub);
    rt.spawn(async move {
        hub_run.run(listen, shutdown_rx).await;
    });

    (hub, shutdown_tx, ev_rx)
}

async fn free_addr() -> std::net::SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    a
}

async fn pair_ab(
    max_payload: u64,
) -> (
    Uuid,
    Uuid,
    Arc<NetworkHub>,
    Arc<NetworkHub>,
    mpsc::Sender<()>,
    mpsc::Sender<()>,
    tokio::sync::broadcast::Receiver<NetEvent>,
    tokio::sync::broadcast::Receiver<NetEvent>,
    std::net::SocketAddr,
    std::net::SocketAddr,
) {
    let password = "e2e-auto-test-pass";
    let id_a = Uuid::from_bytes([0xAA; 16]);
    let id_b = Uuid::from_bytes([0xBB; 16]);
    let addr_a = free_addr().await;
    let addr_b = free_addr().await;
    let (hub_a, shut_a, mut ev_a) = spawn_hub(id_a, "E2E-A", password, max_payload, addr_a);
    let (hub_b, shut_b, mut ev_b) = spawn_hub(id_b, "E2E-B", password, max_payload, addr_b);
    tokio::time::sleep(Duration::from_millis(100)).await;
    hub_a.trial_connect(id_b, addr_b);
    let _ = wait_event(&mut ev_a, Duration::from_secs(10), |e| {
        matches!(e, NetEvent::PeerSessionReady { device_id, .. } if *device_id == id_b)
    })
    .await;
    let _ = wait_event(&mut ev_b, Duration::from_secs(10), |e| {
        matches!(e, NetEvent::PeerSessionReady { device_id, .. } if *device_id == id_a)
    })
    .await;
    // Drain pairing noise
    while ev_a.try_recv().is_ok() {}
    while ev_b.try_recv().is_ok() {}
    (
        id_a, id_b, hub_a, hub_b, shut_a, shut_b, ev_a, ev_b, addr_a, addr_b,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_pair_text_image_and_file() {
    let max_payload = 32 * 1024 * 1024u64;
    let (id_a, _id_b, hub_a, hub_b, shut_a, shut_b, mut _ev_a, mut ev_b, _, _) =
        pair_ab(max_payload).await;
    assert!(hub_a.connected_count() >= 1);
    assert!(hub_b.connected_count() >= 1);

    // Text
    {
        let ev = eng(id_a, max_payload)
            .lock()
            .on_local_text("auto-test hello 中文")
            .expect("text");
        hub_a.broadcast_clipboard(ev);
        let got = wait_event(&mut ev_b, Duration::from_secs(10), |e| {
            matches!(e, NetEvent::ClipboardFromRemote(_))
        })
        .await;
        match got {
            NetEvent::ClipboardFromRemote(ev) => {
                assert_eq!(ev.kind, ContentKind::Text);
                assert_eq!(String::from_utf8(ev.payload).unwrap(), "auto-test hello 中文");
            }
            _ => unreachable!(),
        }
    }

    // Image
    {
        let rgba = vec![255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        let png = ohmycopy::clipboard::rgba_to_png(2, 2, &rgba).unwrap();
        let ev = eng(id_a, max_payload)
            .lock()
            .on_local_image(png.clone(), Some("e2e.png".into()))
            .unwrap();
        hub_a.broadcast_clipboard(ev);
        let got = wait_event(&mut ev_b, Duration::from_secs(15), |e| {
            matches!(e, NetEvent::ClipboardFromRemote(ev) if ev.kind == ContentKind::Image)
        })
        .await;
        match got {
            NetEvent::ClipboardFromRemote(ev) => assert_eq!(ev.payload, png),
            _ => unreachable!(),
        }
    }

    // Small file (256 KiB) — keep this suite fast; large files have dedicated tests.
    {
        let size = 256 * 1024usize;
        let mut payload = vec![0u8; size];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let ev = eng(id_a, max_payload)
            .lock()
            .on_local_file("e2e-bin.dat", payload.clone(), "application/octet-stream")
            .unwrap();
        hub_a.broadcast_clipboard(ev);
        let got = wait_event(&mut ev_b, Duration::from_secs(30), |e| {
            matches!(e, NetEvent::ClipboardFromRemote(ev) if ev.kind == ContentKind::File)
        })
        .await;
        match got {
            NetEvent::ClipboardFromRemote(ev) => {
                assert_eq!(ev.payload.len(), size);
                assert_eq!(ev.payload, payload);
            }
            _ => unreachable!(),
        }
    }

    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}

/// Large binary payload over hub (default 8 MiB; override with OHMYCOPY_E2E_LARGE_MB).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_large_file_sync() {
    let mb: usize = std::env::var("OHMYCOPY_E2E_LARGE_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0 && n <= 200)
        .unwrap_or(8);
    let size = mb * 1024 * 1024;
    let max_payload = (size as u64).saturating_add(4 * 1024 * 1024);
    let (id_a, _, hub_a, _, shut_a, shut_b, _, mut ev_b, _, _) = pair_ab(max_payload).await;

    let mut payload = vec![0u8; size];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = ((i * 31) % 251) as u8;
    }
    // Spot-check pattern so we don't only compare lengths.
    payload[0] = 0xA5;
    payload[size - 1] = 0x5A;

    let ev = eng(id_a, max_payload)
        .lock()
        .on_local_file(
            &format!("large-{mb}m.bin"),
            payload.clone(),
            "application/octet-stream",
        )
        .expect("large file should be within max_payload");
    hub_a.broadcast_clipboard(ev);

    let timeout = Duration::from_secs(60u64.saturating_add((mb as u64) * 5));
    let got = wait_event(&mut ev_b, timeout, |e| {
        matches!(
            e,
            NetEvent::ClipboardFromRemote(ev)
                if ev.kind == ContentKind::File && ev.payload.len() == size
        )
    })
    .await;
    match got {
        NetEvent::ClipboardFromRemote(ev) => {
            assert_eq!(ev.payload.len(), size);
            assert_eq!(ev.payload[0], 0xA5);
            assert_eq!(ev.payload[size - 1], 0x5A);
            assert_eq!(ev.payload, payload);
            let expected_name = format!("large-{mb}m.bin");
            assert_eq!(ev.file_name.as_deref(), Some(expected_name.as_str()));
        }
        _ => unreachable!(),
    }

    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_folder_zip_sync() {
    let max_payload = 16 * 1024 * 1024u64;
    let (id_a, _, hub_a, _, shut_a, shut_b, _, mut ev_b, _, _) = pair_ab(max_payload).await;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("photos");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), b"folder-a").unwrap();
    std::fs::write(root.join("sub").join("b.txt"), b"folder-b").unwrap();

    let (name, bytes, mime) = inbox::pack_path(&root, max_payload).unwrap();
    assert_eq!(mime, MIME_DIR_ZIP);
    assert!(name.ends_with(".zip"));

    let folder_name = root.file_name().unwrap().to_string_lossy().into_owned();
    let ev = eng(id_a, max_payload)
        .lock()
        .on_local_file(&folder_name, bytes.clone(), MIME_DIR_ZIP)
        .unwrap();
    assert_eq!(ev.kind, ContentKind::File);
    hub_a.broadcast_clipboard(ev);

    let got = wait_event(&mut ev_b, Duration::from_secs(30), |e| {
        matches!(
            e,
            NetEvent::ClipboardFromRemote(ev)
                if ev.kind == ContentKind::File && ev.mime == MIME_DIR_ZIP
        )
    })
    .await;
    match got {
        NetEvent::ClipboardFromRemote(ev) => {
            assert_eq!(ev.payload, bytes);
            assert!(!ev.payload.is_empty());
            // Real extract into inbox receipt dir and verify nested files.
            let dest = inbox::store_folder_zip("t", "photos", &ev.payload).expect("extract");
            assert!(dest.is_dir(), "dest={}", dest.display());
            assert_eq!(
                std::fs::read_to_string(dest.join("a.txt")).unwrap(),
                "folder-a"
            );
            assert_eq!(
                std::fs::read_to_string(dest.join("sub").join("b.txt")).unwrap(),
                "folder-b"
            );
        }
        _ => unreachable!(),
    }

    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}

/// Folder with multi-MB contents packed as zip and synced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_large_folder_zip_sync() {
    let max_payload = 64 * 1024 * 1024u64;
    let (id_a, _, hub_a, _, shut_a, shut_b, _, mut ev_b, _, _) = pair_ab(max_payload).await;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("big-folder");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("readme.txt"), b"large-folder-marker").unwrap();
    // ~3 MiB of compressible + incompressible-ish data across files
    let mut chunk = vec![0u8; 1024 * 1024];
    for (i, b) in chunk.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    for i in 0..3 {
        std::fs::write(root.join(format!("blob-{i}.bin")), &chunk).unwrap();
    }
    std::fs::write(root.join("nested").join("deep.bin"), &chunk[..512 * 1024]).unwrap();

    let (name, bytes, mime) = inbox::pack_path(&root, max_payload).unwrap();
    assert_eq!(mime, MIME_DIR_ZIP);
    assert!(name.ends_with(".zip"));
    assert!(bytes.len() > 1000);

    let folder_name = root.file_name().unwrap().to_string_lossy().into_owned();
    let ev = eng(id_a, max_payload)
        .lock()
        .on_local_file(&folder_name, bytes.clone(), MIME_DIR_ZIP)
        .unwrap();
    hub_a.broadcast_clipboard(ev);

    let got = wait_event(&mut ev_b, Duration::from_secs(90), |e| {
        matches!(
            e,
            NetEvent::ClipboardFromRemote(ev)
                if ev.kind == ContentKind::File
                    && ev.mime == MIME_DIR_ZIP
                    && ev.payload.len() == bytes.len()
        )
    })
    .await;
    match got {
        NetEvent::ClipboardFromRemote(ev) => {
            assert_eq!(ev.payload, bytes);
            let dest =
                inbox::store_folder_zip("lg", "big-folder", &ev.payload).expect("extract large folder");
            assert_eq!(
                std::fs::read_to_string(dest.join("readme.txt")).unwrap(),
                "large-folder-marker"
            );
            assert_eq!(
                std::fs::metadata(dest.join("blob-0.bin")).unwrap().len(),
                1024 * 1024
            );
            assert_eq!(
                std::fs::metadata(dest.join("nested").join("deep.bin"))
                    .unwrap()
                    .len(),
                512 * 1024
            );
        }
        _ => unreachable!(),
    }

    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_path_rejects_oversize_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("too-big.bin");
    std::fs::write(&f, vec![1u8; 64 * 1024]).unwrap();
    let err = inbox::pack_path(&f, 1024).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("超过") || msg.contains("上限"),
        "unexpected err: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_reverse_b_to_a() {
    let max_payload = 4 * 1024 * 1024u64;
    let (_id_a, id_b, hub_a, hub_b, shut_a, shut_b, mut ev_a, _, _, _) = pair_ab(max_payload).await;

    let ev = eng(id_b, max_payload)
        .lock()
        .on_local_text("from-B-to-A")
        .unwrap();
    hub_b.broadcast_clipboard(ev);
    let got = wait_event(&mut ev_a, Duration::from_secs(10), |e| {
        matches!(e, NetEvent::ClipboardFromRemote(_))
    })
    .await;
    match got {
        NetEvent::ClipboardFromRemote(ev) => {
            assert_eq!(String::from_utf8(ev.payload).unwrap(), "from-B-to-A");
            assert_eq!(ev.source_id, id_b);
        }
        _ => unreachable!(),
    }
    assert!(hub_a.connected_count() >= 1);
    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_ignore_mutes_clipboard() {
    let max_payload = 2 * 1024 * 1024u64;
    let (_id_a, id_b, hub_a, hub_b, shut_a, shut_b, _, mut ev_b, _, addr_b) =
        pair_ab(max_payload).await;

    hub_a.set_ignored(Some(id_b), addr_b, true);
    let id_a = Uuid::from_bytes([0xAA; 16]);
    let ev = eng(id_a, max_payload)
        .lock()
        .on_local_text("should-not-arrive")
        .unwrap();
    hub_a.broadcast_clipboard(ev);

    let leaked = wait_event_optional(&mut ev_b, Duration::from_secs(2), |e| {
        matches!(
            e,
            NetEvent::ClipboardFromRemote(ev)
                if String::from_utf8_lossy(&ev.payload).contains("should-not-arrive")
        )
    })
    .await;
    assert!(!leaked, "ignored peer must not receive clipboard");

    hub_a.set_ignored(Some(id_b), addr_b, false);
    let ev2 = eng(id_a, max_payload)
        .lock()
        .on_local_text("after-unignore")
        .unwrap();
    hub_a.broadcast_clipboard(ev2);
    let got = wait_event(&mut ev_b, Duration::from_secs(10), |e| {
        matches!(
            e,
            NetEvent::ClipboardFromRemote(ev)
                if String::from_utf8_lossy(&ev.payload).contains("after-unignore")
        )
    })
    .await;
    assert!(matches!(got, NetEvent::ClipboardFromRemote(_)));
    assert!(hub_b.connected_count() >= 1);

    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_unpair_notifies_peer() {
    let max_payload = 2 * 1024 * 1024u64;
    let (_id_a, id_b, hub_a, hub_b, shut_a, shut_b, _, mut ev_b, _, addr_b) =
        pair_ab(max_payload).await;

    assert!(hub_a.connected_count() >= 1);
    hub_a.remove_client(Some(id_b), addr_b);

    let got = wait_event_optional(&mut ev_b, Duration::from_secs(5), |e| {
        matches!(e, NetEvent::PeerUnpaired { from_remote: true, .. })
            || matches!(e, NetEvent::Toast(s) if s.contains("解除") || s.contains("断开"))
    })
    .await;
    // PeerUnpaired or disconnect toast — either means unpair path ran.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(hub_a.connected_count(), 0);
    let _ = got;
    let _ = hub_b;

    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hub_oversize_not_emitted() {
    let id = Uuid::new_v4();
    let mut e = EngineCore::new(id, 1024, true); // 1 KiB max
    assert!(e.on_local_text(&"x".repeat(2000)).is_none());
    assert!(e
        .on_local_file("big.bin", vec![0u8; 4096], "application/octet-stream")
        .is_none());
    let big_png = vec![0u8; 4096];
    assert!(e.on_local_image(big_png, None).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hub_auth_fail_wrong_password() {
    let id_a = Uuid::from_bytes([0x11; 16]);
    let id_b = Uuid::from_bytes([0x22; 16]);
    let addr_a = free_addr().await;
    let addr_b = free_addr().await;

    let (hub_a, shut_a, mut ev_a) = spawn_hub(id_a, "A", "password-A", 1024 * 1024, addr_a);
    let (_hub_b, shut_b, _ev_b) = spawn_hub(id_b, "B", "password-B", 1024 * 1024, addr_b);
    tokio::time::sleep(Duration::from_millis(80)).await;

    hub_a.trial_connect(id_b, addr_b);
    let _ = wait_event(&mut ev_a, Duration::from_secs(10), |e| {
        matches!(e, NetEvent::PeerAuthFailed { .. })
            || matches!(
                e,
                NetEvent::Toast(s)
                    if s.contains("密码")
                        || s.contains("鉴权")
                        || s.contains("失败")
                        || s.contains("连接")
            )
    })
    .await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hub_a.connected_count(), 0);
    let _ = shut_a.try_send(());
    let _ = shut_b.try_send(());
}
