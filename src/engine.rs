use crate::protocol::{ClipboardEvent, ContentKind};
use parking_lot::Mutex;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};
use uuid::Uuid;

const SEEN_CAP: usize = 4096;

/// event_id seen set (LRU-ish via queue + set).
pub struct SeenSet {
    set: HashSet<Uuid>,
    order: VecDeque<Uuid>,
}

impl Default for SeenSet {
    fn default() -> Self {
        Self {
            set: HashSet::with_capacity(SEEN_CAP),
            order: VecDeque::with_capacity(SEEN_CAP),
        }
    }
}

impl SeenSet {
    pub fn insert_if_new(&mut self, id: Uuid) -> bool {
        if self.set.contains(&id) {
            return false;
        }
        if self.order.len() >= SEEN_CAP {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        self.set.insert(id);
        self.order.push_back(id);
        true
    }

    pub fn contains(&self, id: &Uuid) -> bool {
        self.set.contains(id)
    }
}

/// Tracks recent sync writes so clipboard watcher does not re-broadcast.
pub struct SyncGuard {
    until: Instant,
    last_key: Option<([u8; 32], ContentKind)>,
}

impl Default for SyncGuard {
    fn default() -> Self {
        Self {
            until: Instant::now(),
            last_key: None,
        }
    }
}

impl SyncGuard {
    pub fn mark_sync_write(&mut self, hash: [u8; 32], kind: ContentKind, cooldown: Duration) {
        self.until = Instant::now() + cooldown;
        self.last_key = Some((hash, kind));
    }

    pub fn should_suppress(&self, hash: &[u8; 32], kind: ContentKind) -> bool {
        if Instant::now() < self.until {
            if let Some(key) = self.last_key {
                return key == (*hash, kind);
            }
            return true;
        }
        false
    }
}

pub struct EngineCore {
    pub seen: SeenSet,
    pub sync_guard: SyncGuard,
    pub local_device_id: Uuid,
    pub sync_enabled: bool,
    pub max_payload_bytes: u64,
}

impl EngineCore {
    pub fn new(local_device_id: Uuid, max_payload_bytes: u64, sync_enabled: bool) -> Self {
        Self {
            seen: SeenSet::default(),
            sync_guard: SyncGuard::default(),
            local_device_id,
            sync_enabled,
            max_payload_bytes,
        }
    }

    /// Prepare outbound event from local clipboard change. None if suppressed.
    pub fn on_local_text(&mut self, text: &str) -> Option<ClipboardEvent> {
        if !self.sync_enabled {
            return None;
        }
        let payload = text.as_bytes().to_vec();
        if payload.len() as u64 > self.max_payload_bytes {
            return None; // caller should notify UI about limit
        }
        let content_hash = content_hash(ContentKind::Text, &payload);
        if self
            .sync_guard
            .should_suppress(&content_hash, ContentKind::Text)
        {
            return None;
        }
        let event_id = Uuid::new_v4();
        self.seen.insert_if_new(event_id);
        Some(ClipboardEvent {
            event_id,
            source_id: self.local_device_id,
            content_hash,
            kind: crate::protocol::ContentKind::Text,
            mime: "text/plain; charset=utf-8".into(),
            created_at: now_ms(),
            file_name: None,
            payload,
        })
    }

    /// Prepare outbound file event from local Explorer copy. None if suppressed.
    pub fn on_local_file(
        &mut self,
        file_name: &str,
        payload: Vec<u8>,
        mime: &str,
    ) -> Option<ClipboardEvent> {
        if !self.sync_enabled {
            return None;
        }
        if payload.len() as u64 > self.max_payload_bytes {
            return None;
        }
        let content_hash = content_hash(ContentKind::File, &payload);
        if self
            .sync_guard
            .should_suppress(&content_hash, ContentKind::File)
        {
            return None;
        }
        let event_id = Uuid::new_v4();
        self.seen.insert_if_new(event_id);
        Some(ClipboardEvent {
            event_id,
            source_id: self.local_device_id,
            content_hash,
            kind: crate::protocol::ContentKind::File,
            mime: mime.into(),
            created_at: now_ms(),
            file_name: Some(file_name.to_string()),
            payload,
        })
    }

    /// Prepare outbound image event (PNG payload). None if suppressed / oversize.
    pub fn on_local_image(
        &mut self,
        png_payload: Vec<u8>,
        file_name: Option<String>,
    ) -> Option<ClipboardEvent> {
        if !self.sync_enabled {
            return None;
        }
        if png_payload.len() as u64 > self.max_payload_bytes {
            return None;
        }
        let content_hash = content_hash(ContentKind::Image, &png_payload);
        if self
            .sync_guard
            .should_suppress(&content_hash, ContentKind::Image)
        {
            return None;
        }
        let event_id = Uuid::new_v4();
        self.seen.insert_if_new(event_id);
        Some(ClipboardEvent {
            event_id,
            source_id: self.local_device_id,
            content_hash,
            kind: crate::protocol::ContentKind::Image,
            mime: "image/png".into(),
            created_at: now_ms(),
            file_name,
            payload: png_payload,
        })
    }

    /// Returns true if this is a new event that should be applied locally
    /// and relayed to other peers. Duplicate event_id → false (no re-apply / no re-relay).
    pub fn on_remote_event(&mut self, ev: &ClipboardEvent) -> bool {
        if !self.sync_enabled {
            return false;
        }
        if !self.seen.insert_if_new(ev.event_id) {
            return false;
        }
        if ev.payload.len() as u64 > self.max_payload_bytes {
            return false;
        }
        // Suppress local clipboard watcher so applying the text does not
        // create a *new* outbound event (relay uses the original event_id).
        self.sync_guard
            .mark_sync_write(ev.content_hash, ev.kind, Duration::from_millis(800));
        true
    }

    pub fn note_oversize_local(&self, len: u64) -> bool {
        len > self.max_payload_bytes
    }
}

/// Clipboard payload bytes do not describe their user-visible semantics.  For
/// example, a PNG copied as a bitmap and the same PNG copied from Explorer are
/// different clipboard entries: one must paste pixels, the other a file.
/// Domain-separate hashes so synchronization guards and future deduplication
/// cannot merge those entries merely because their bytes happen to match.
fn content_hash(kind: ContentKind, payload: &[u8]) -> [u8; 32] {
    let tag = match kind {
        ContentKind::Text => b"text".as_slice(),
        ContentKind::File => b"file".as_slice(),
        ContentKind::Image => b"image".as_slice(),
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag);
    hasher.update(&[0]);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub type SharedEngine = std::sync::Arc<Mutex<EngineCore>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_set_dedup() {
        let mut s = SeenSet::default();
        let id = Uuid::new_v4();
        assert!(s.insert_if_new(id));
        assert!(!s.insert_if_new(id));
    }

    #[test]
    fn local_then_remote_same_event_id_dropped() {
        let id = Uuid::new_v4();
        let mut eng = EngineCore::new(id, 1024 * 1024, true);
        let ev = eng.on_local_text("hello").unwrap();
        assert!(!eng.on_remote_event(&ev));
    }

    #[test]
    fn sync_guard_suppresses_echo() {
        let id = Uuid::new_v4();
        let mut eng = EngineCore::new(id, 1024 * 1024, true);
        let hash = *blake3::hash(b"x").as_bytes();
        eng.sync_guard
            .mark_sync_write(hash, ContentKind::Image, Duration::from_secs(5));
        assert!(eng.sync_guard.should_suppress(&hash, ContentKind::Image));
        assert!(!eng.sync_guard.should_suppress(&hash, ContentKind::File));
        assert!(!eng
            .sync_guard
            .should_suppress(blake3::hash(b"y").as_bytes(), ContentKind::Image));
    }

    #[test]
    fn equal_bytes_have_distinct_hashes_for_distinct_clipboard_kinds() {
        assert_ne!(
            content_hash(ContentKind::Image, b"png bytes"),
            content_hash(ContentKind::File, b"png bytes")
        );
    }

    #[test]
    fn oversize_rejected() {
        let id = Uuid::new_v4();
        let mut eng = EngineCore::new(id, 4, true);
        assert!(eng.on_local_text("hello").is_none());
    }

    #[test]
    fn remote_event_once_then_dedup() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut eng = EngineCore::new(a, 1024 * 1024, true);
        let mut src = EngineCore::new(b, 1024 * 1024, true);
        let ev = src.on_local_text("relay-me").unwrap();
        assert!(eng.on_remote_event(&ev));
        assert!(!eng.on_remote_event(&ev));
    }
}
