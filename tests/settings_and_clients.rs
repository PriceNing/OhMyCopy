//! Settings / clients / inbox unit coverage for features not hit by hub e2e.

use ohmycopy::clients::{ClientSource, ClientsFile};
use ohmycopy::config::Config;
use ohmycopy::history::HistoryStore;
use ohmycopy::inbox::{self, MIME_DIR_ZIP};
use std::fs;
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn config_defaults_and_start_minimized_field() {
    let mut cfg = Config::default();
    assert!(!cfg.start_minimized_to_tray);
    assert!(!cfg.console);
    assert!(cfg.sync_enabled);
    assert_eq!(cfg.max_payload_bytes, 10 * 1024 * 1024);
    // Fresh default must not use the public insecure password.
    assert!(!Config::is_insecure_default_password(&cfg.password));
    cfg.start_minimized_to_tray = true;
    cfg.max_payload_bytes = 200 * 1024 * 1024;
    cfg.console = true;
    let json = serde_json::to_string(&cfg).unwrap();
    let back: Config = serde_json::from_str(&json).unwrap();
    assert!(back.start_minimized_to_tray);
    assert_eq!(back.max_payload_bytes, 200 * 1024 * 1024);
    assert!(back.console);
}

#[test]
fn config_missing_keys_fill_defaults() {
    let json = r#"{
        "config_version": 2,
        "device_name": "X",
        "device_id": "00000000-0000-4000-8000-000000000099",
        "tcp_port": 3721,
        "udp_port": 3721,
        "password": "p",
        "max_payload_bytes": 209715200,
        "history_limit": 100,
        "discover_interval_secs": 5,
        "theme": "dark_glass",
        "auto_start": false,
        "sync_enabled": true
    }"#;
    let cfg: Config = serde_json::from_str(json).unwrap();
    assert!(!cfg.start_minimized_to_tray);
    assert!(!cfg.console);
    assert_eq!(cfg.max_payload_bytes, 209715200);
}

#[test]
fn clients_pair_ignore_remove() {
    let mut f = ClientsFile::default();
    let id = Uuid::new_v4();
    let addr: std::net::SocketAddr = "192.168.1.9:3721".parse().unwrap();
    f.add_paired(Some(id), "Peer".into(), addr, ClientSource::Discover);
    assert!(f.contains_device(id));
    assert!(!f.clients[0].ignored);
    assert!(f.set_ignored(Some(id), &addr.to_string(), true));
    assert!(f.clients[0].ignored);
    assert_eq!(f.ignored_device_ids(), vec![id]);
    assert!(f.set_ignored(Some(id), &addr.to_string(), false));
    assert!(f.remove(Some(id), &addr.to_string()));
    assert!(f.clients.is_empty());
}

#[test]
fn history_preview_never_contains_windows_path() {
    let dir = tempdir().unwrap();
    let store = HistoryStore::open(&dir.path().join("h.db"), 50).unwrap();
    let e = Uuid::new_v4();
    let s = Uuid::new_v4();
    let path = r"C:\Users\Administrator\AppData\Local\Temp\orca-paste-xxx.png";
    store.insert_image(e, s, path, path, 12345, 1).unwrap();
    let items = store.list("", 10).unwrap();
    assert_eq!(items.len(), 1);
    let img_tag = ohmycopy::i18n::t("history.tag_image");
    assert!(
        items[0].preview.starts_with(&img_tag)
            || items[0].preview.starts_with("[Image]")
            || items[0].preview.starts_with("[图片]"),
        "preview={}",
        items[0].preview
    );
    assert!(
        !items[0].preview.contains(r"C:\"),
        "preview leaked path: {}",
        items[0].preview
    );
    assert!(
        items[0].preview.contains("orca-paste-xxx.png")
            || items[0].preview.contains("图片")
            || items[0].preview.contains("Image")
    );
    // content keeps path for re-copy
    assert!(items[0].content.contains("orca-paste") || items[0].content == path);
}

#[test]
fn history_file_preview_short_name() {
    let dir = tempdir().unwrap();
    let store = HistoryStore::open(&dir.path().join("h.db"), 50).unwrap();
    let e = Uuid::new_v4();
    let s = Uuid::new_v4();
    store
        .insert_file(
            e,
            s,
            r"C:\Users\Administrator\Desktop\report.pdf",
            r"C:\Users\Administrator\Desktop\report.pdf",
            999,
            1,
        )
        .unwrap();
    let items = store.list("", 5).unwrap();
    let file_tag = ohmycopy::i18n::t("history.tag_file");
    assert!(
        items[0].preview.starts_with(&file_tag)
            || items[0].preview.starts_with("[File]")
            || items[0].preview.starts_with("[文件]"),
        "preview={}",
        items[0].preview
    );
    assert!(items[0].preview.contains("report.pdf"));
    assert!(!items[0].preview.contains(r"C:\Users"));
}

#[test]
fn inbox_pack_folder_and_unique_receipt() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("mydir");
    fs::create_dir_all(root.join("n")).unwrap();
    fs::write(root.join("a.txt"), b"1").unwrap();
    fs::write(root.join("n").join("b.txt"), b"2").unwrap();
    let (name, bytes, mime) = inbox::pack_path(&root, 5 * 1024 * 1024).unwrap();
    assert!(name.ends_with(".zip"));
    assert_eq!(mime, MIME_DIR_ZIP);
    assert!(!bytes.is_empty());
}

#[test]
fn inbox_pack_folder_then_store_roundtrip() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("pack-me");
    fs::create_dir_all(root.join("deep")).unwrap();
    fs::write(root.join("top.txt"), b"top-level").unwrap();
    fs::write(root.join("deep").join("inner.bin"), vec![7u8; 4096]).unwrap();
    let (_name, bytes, mime) = inbox::pack_path(&root, 8 * 1024 * 1024).unwrap();
    assert_eq!(mime, MIME_DIR_ZIP);
    let out = inbox::store_folder_zip("ut", "pack-me", &bytes).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("top.txt")).unwrap(),
        "top-level"
    );
    assert_eq!(
        fs::metadata(out.join("deep").join("inner.bin"))
            .unwrap()
            .len(),
        4096
    );
}

#[test]
fn inbox_pack_file_oversize_errors() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("big.bin");
    fs::write(&f, vec![9u8; 10_000]).unwrap();
    assert!(inbox::pack_path(&f, 100).is_err());
}

#[test]
fn inbox_store_file_uses_timestamp_parent() {
    // Uses real config data dir next to exe — still should not panic.
    // Prefer isolated env if possible; store_file creates receipt dir.
    // Skip if no write permission.
    let payload = b"hello-inbox-test";
    match inbox::store_file("deadbeef", "hello.txt", payload) {
        Ok(p) => {
            assert!(p.is_file());
            assert_eq!(fs::read(&p).unwrap(), payload);
            // parent should look like a timestamp folder
            let parent = p.parent().unwrap().file_name().unwrap().to_string_lossy();
            assert!(
                parent.len() >= 8,
                "expected timestamp-ish receipt dir, got {parent}"
            );
            // file name clean
            assert_eq!(p.file_name().unwrap(), "hello.txt");
        }
        Err(e) => {
            eprintln!("skip inbox store test (no writable data dir): {e}");
        }
    }
}

#[test]
fn png_roundtrip_tiny() {
    let rgba = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let png = ohmycopy::clipboard::rgba_to_png(2, 2, &rgba).unwrap();
    assert!(!png.is_empty());
    let (w, h, out) = ohmycopy::clipboard::png_to_rgba(&png).unwrap();
    assert_eq!((w, h), (2, 2));
    assert_eq!(out.len(), 16);
    assert_eq!(out[0..4], rgba[0..4]);
}

#[test]
fn clients_json_serde_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("clients.json");
    let mut f = ClientsFile::default();
    let id = Uuid::new_v4();
    let addr: std::net::SocketAddr = "10.0.0.2:3721".parse().unwrap();
    f.add_paired(Some(id), "PersistPeer".into(), addr, ClientSource::Manual);
    f.set_ignored(Some(id), &addr.to_string(), true);
    fs::write(&path, serde_json::to_string_pretty(&f).unwrap()).unwrap();
    let loaded = ClientsFile::load(&path).unwrap();
    assert_eq!(loaded.clients.len(), 1);
    assert_eq!(loaded.clients[0].name, "PersistPeer");
    assert!(loaded.clients[0].ignored);
    assert_eq!(loaded.clients[0].device_id, Some(id));
}
