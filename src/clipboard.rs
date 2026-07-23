//! Clipboard access is serialized on a dedicated OS thread.
//! Supports text, images/screenshots, and file lists (Windows CF_HDROP).

use anyhow::{bail, Context, Result};
use arboard::{Clipboard, ImageData};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Snapshot of what we care about on the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    Empty,
    Text(String),
    /// Absolute paths (Explorer / file manager copy).
    Files(Vec<PathBuf>),
    /// RGBA8 pixels (screenshot / copy image).
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

impl ClipContent {
    pub fn fingerprint(&self) -> String {
        match self {
            ClipContent::Empty => String::new(),
            ClipContent::Text(t) => format!("t:{}", t),
            ClipContent::Files(paths) => {
                let mut s = String::from("f:");
                for p in paths {
                    s.push_str(&p.to_string_lossy());
                    s.push('\n');
                }
                s
            }
            ClipContent::Image {
                width,
                height,
                rgba,
            } => {
                // Hash pixels so identical screenshots suppress; size changes always fire.
                let h = blake3::hash(rgba);
                format!("i:{}x{}:{}", width, height, h.to_hex())
            }
        }
    }
}

enum ClipRequest {
    Get {
        reply: SyncSender<Result<ClipContent>>,
    },
    SetText {
        text: String,
        from_sync: bool,
        reply: SyncSender<Result<()>>,
    },
    SetFiles {
        paths: Vec<PathBuf>,
        from_sync: bool,
        reply: SyncSender<Result<()>>,
    },
    SetImage {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        from_sync: bool,
        reply: SyncSender<Result<()>>,
    },
}

/// Thread-safe clipboard: all OS calls run on one background thread.
pub struct ClipboardService {
    tx: SyncSender<ClipRequest>,
    /// Fingerprint of content we just wrote from a remote sync. Watcher must only
    /// ignore a change that matches this fingerprint — a broad "suppress next"
    /// flag can swallow a *later* real user copy if the sync poll is delayed.
    suppress_fp: Arc<Mutex<Option<String>>>,
}

/// The Win32 clipboard is scoped to an interactive desktop.  Clipboard writes
/// must therefore run in the same process that owns the user's desktop, rather
/// than in an arbitrary worker spawned by a GUI-subsystem executable.
#[cfg(windows)]
fn attach_to_input_desktop() {
    const GENERIC_ALL: u32 = 0x1000_0000;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> *mut core::ffi::c_void;
        fn SetThreadDesktop(desktop: *mut core::ffi::c_void) -> i32;
    }

    unsafe {
        let desktop = OpenInputDesktop(0, 0, GENERIC_ALL);
        if !desktop.is_null() && SetThreadDesktop(desktop) == 0 {
            tracing::debug!("SetThreadDesktop(OpenInputDesktop) failed");
        }
        // Do not close `desktop`: the successful SetThreadDesktop call makes it
        // the thread's desktop and the handle must stay valid for that thread.
    }
}

#[cfg(not(windows))]
fn attach_to_input_desktop() {}

impl ClipboardService {
    pub fn new() -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<ClipRequest>(32);
        let suppress_fp = Arc::new(Mutex::new(None));
        let suppress_flag = Arc::clone(&suppress_fp);

        thread::Builder::new()
            .name("ohmycopy-clipboard".into())
            .spawn(move || clipboard_thread(rx, suppress_flag))
            .context("spawn clipboard thread")?;

        Ok(Self { tx, suppress_fp })
    }

    fn mark_suppress_fp(&self, fp: String) {
        *self.suppress_fp.lock() = Some(fp);
    }

    fn clear_suppress_fp(&self) {
        *self.suppress_fp.lock() = None;
    }

    pub fn get(&self) -> Result<ClipContent> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(ClipRequest::Get { reply: reply_tx })
            .map_err(|_| anyhow::anyhow!("clipboard thread dead"))?;
        match reply_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(r) => r,
            Err(_) => bail!("clipboard get timeout"),
        }
    }

    pub fn get_text(&self) -> Result<String> {
        match self.get()? {
            ClipContent::Text(t) => Ok(t),
            _ => Ok(String::new()),
        }
    }

    pub fn set_text_from_sync(&self, text: &str) -> Result<()> {
        self.mark_suppress_fp(ClipContent::Text(text.to_string()).fingerprint());
        self.set_text_inner(text, true)
    }

    pub fn set_text_local(&self, text: &str) -> Result<()> {
        self.set_text_inner(text, false)
    }

    fn set_text_inner(&self, text: &str, from_sync: bool) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(ClipRequest::SetText {
                text: text.to_string(),
                from_sync,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("clipboard thread dead"))?;
        match reply_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(r) => r,
            Err(_) => {
                if from_sync {
                    self.clear_suppress_fp();
                }
                bail!("clipboard set_text timeout")
            }
        }
    }

    pub fn set_files_from_sync(&self, paths: &[PathBuf]) -> Result<()> {
        self.mark_suppress_fp(ClipContent::Files(paths.to_vec()).fingerprint());
        self.set_files_inner(paths, true)
    }

    /// Put a file list on the local clipboard without suppressing the watcher.
    /// This is used by local callers (including the clipboard probe) that want
    /// the normal outbound-sync path to observe the CF_HDROP change.
    pub fn set_files_local(&self, paths: &[PathBuf]) -> Result<()> {
        self.set_files_inner(paths, false)
    }

    fn set_files_inner(&self, paths: &[PathBuf], from_sync: bool) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(ClipRequest::SetFiles {
                paths: paths.to_vec(),
                from_sync,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("clipboard thread dead"))?;
        match reply_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(r) => r,
            Err(_) => {
                if from_sync {
                    self.clear_suppress_fp();
                }
                bail!("clipboard set_files timeout")
            }
        }
    }

    pub fn set_image_from_sync(&self, width: u32, height: u32, rgba: Vec<u8>) -> Result<()> {
        self.mark_suppress_fp(
            ClipContent::Image {
                width,
                height,
                rgba: rgba.clone(),
            }
            .fingerprint(),
        );
        self.set_image_inner(width, height, rgba, true)
    }

    pub fn set_image_local(&self, width: u32, height: u32, rgba: Vec<u8>) -> Result<()> {
        self.set_image_inner(width, height, rgba, false)
    }

    fn set_image_inner(
        &self,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        from_sync: bool,
    ) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(ClipRequest::SetImage {
                width,
                height,
                rgba,
                from_sync,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("clipboard thread dead"))?;
        match reply_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(r) => r,
            Err(_) => {
                if from_sync {
                    self.clear_suppress_fp();
                }
                bail!("clipboard set_image timeout")
            }
        }
    }

    /// Returns true if `fp` is the fingerprint of a remote sync write we should ignore.
    /// Only the matching fingerprint is consumed; a later different copy still fires.
    pub fn take_suppress_for(&self, fp: &str) -> bool {
        let mut g = self.suppress_fp.lock();
        if g.as_deref() == Some(fp) {
            *g = None;
            true
        } else {
            false
        }
    }

    /// Backward-compatible: clear any pending suppress (e.g. tests / probe).
    pub fn take_suppress(&self) -> bool {
        self.suppress_fp.lock().take().is_some()
    }
}

/// Encode RGBA8 → PNG bytes for network / disk.
pub fn rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if rgba.len() < expected {
        bail!(
            "RGBA buffer too small: got {} need {} ({}x{})",
            rgba.len(),
            expected,
            width,
            height
        );
    }
    let img = image::RgbaImage::from_raw(width, height, rgba[..expected].to_vec())
        .ok_or_else(|| anyhow::anyhow!("invalid RGBA image dimensions"))?;
    let mut buf = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut buf);
    image::ImageEncoder::write_image(
        enc,
        img.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .context("encode png")?;
    Ok(buf)
}

/// Decode PNG → (width, height, RGBA8).
pub fn png_to_rgba(png: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(png)
        .context("decode png")?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok((w, h, img.into_raw()))
}

fn clipboard_thread(rx: Receiver<ClipRequest>, suppress_fp: Arc<Mutex<Option<String>>>) {
    attach_to_input_desktop();
    // Keep Option so we can re-open after X11 disconnect / unreachable DISPLAY.
    let mut clipboard: Option<Clipboard> = open_clipboard_handle();
    if clipboard.is_none() {
        tracing::warn!(
            "OS clipboard not available at start (common in Linux headless without \
             a live DISPLAY/Wayland). Will retry and try CLI tools (wl-copy/xclip)."
        );
        #[cfg(target_os = "linux")]
        log_linux_clipboard_hint();
    }

    while let Ok(req) = rx.recv() {
        match req {
            ClipRequest::Get { reply } => {
                let result = read_clip_content_resilient(&mut clipboard);
                let _ = reply.send(result);
            }
            ClipRequest::SetText {
                text,
                from_sync,
                reply,
            } => {
                if from_sync {
                    *suppress_fp.lock() = Some(ClipContent::Text(text.clone()).fingerprint());
                }
                let result = set_text_resilient(&mut clipboard, &text);
                if let Err(e) = &result {
                    tracing::warn!(error = %e, "set clipboard text failed");
                    // Always keep a copy under data dir so headless nodes do not lose payload.
                    if let Err(fe) = persist_last_clip_text(&text) {
                        tracing::debug!(error = %fe, "persist last_clip text");
                    }
                }
                if result.is_err() && from_sync {
                    *suppress_fp.lock() = None;
                }
                let _ = reply.send(result);
            }
            ClipRequest::SetFiles {
                paths,
                from_sync,
                reply,
            } => {
                if from_sync {
                    *suppress_fp.lock() = Some(ClipContent::Files(paths.clone()).fingerprint());
                }
                let result = set_files_resilient(&mut clipboard, &paths);
                if result.is_err() && from_sync {
                    *suppress_fp.lock() = None;
                }
                let _ = reply.send(result);
            }
            ClipRequest::SetImage {
                width,
                height,
                rgba,
                from_sync,
                reply,
            } => {
                if from_sync {
                    *suppress_fp.lock() = Some(
                        ClipContent::Image {
                            width,
                            height,
                            rgba: rgba.clone(),
                        }
                        .fingerprint(),
                    );
                }
                let result = set_image_resilient(&mut clipboard, width, height, &rgba);
                if let Err(e) = &result {
                    tracing::warn!(error = %e, "set clipboard image failed");
                    if let Err(fe) = persist_last_clip_image(width, height, &rgba) {
                        tracing::debug!(error = %fe, "persist last_clip image");
                    }
                }
                if result.is_err() && from_sync {
                    *suppress_fp.lock() = None;
                }
                let _ = reply.send(result);
            }
        }
    }
}

fn open_clipboard_handle() -> Option<Clipboard> {
    match Clipboard::new() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::debug!(error = %e, "Clipboard::new failed");
            None
        }
    }
}

fn ensure_clipboard(clipboard: &mut Option<Clipboard>) -> Result<&mut Clipboard> {
    if clipboard.is_none() {
        *clipboard = open_clipboard_handle();
    }
    clipboard
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("clipboard unavailable: no OS clipboard backend"))
}

fn set_text_resilient(clipboard: &mut Option<Clipboard>, text: &str) -> Result<()> {
    let mut last_err = None;
    for attempt in 0..4 {
        if let Ok(cb) = ensure_clipboard(clipboard) {
            match cb.set_text(text) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("{e}"));
                    // Drop handle so next attempt re-opens (stale X11 socket).
                    *clipboard = None;
                }
            }
        }
        thread::sleep(Duration::from_millis(40 * (attempt + 1) as u64));
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(()) = linux_cli_set_text(text) {
            tracing::info!("clipboard text set via CLI fallback (wl-copy/xclip/xsel)");
            return Ok(());
        }
    }

    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "clipboard unavailable: X11/Wayland unreachable. On Linux headless, run inside \
             the desktop user session (DISPLAY=:0) or install xclip/wl-clipboard."
        )
    }))
}

fn set_image_resilient(
    clipboard: &mut Option<Clipboard>,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<()> {
    let mut last_err = None;
    for attempt in 0..4 {
        if let Ok(cb) = ensure_clipboard(clipboard) {
            let img = ImageData {
                width: width as usize,
                height: height as usize,
                bytes: Cow::Borrowed(rgba),
            };
            match cb.set_image(img) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("{e}"));
                    *clipboard = None;
                }
            }
        }
        thread::sleep(Duration::from_millis(40 * (attempt + 1) as u64));
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(png) = rgba_to_png(width, height, rgba) {
            if linux_cli_set_png(&png).is_ok() {
                tracing::info!("clipboard image set via CLI fallback");
                return Ok(());
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("clipboard image unavailable")))
}

fn read_clip_content_resilient(clipboard: &mut Option<Clipboard>) -> Result<ClipContent> {
    // Prefer OS clipboard; on Linux also try CLI if arboard is down.
    if let Ok(cb) = ensure_clipboard(clipboard) {
        match read_clip_content(cb) {
            Ok(ClipContent::Empty) => {}
            Ok(c) => return Ok(c),
            Err(e) => {
                tracing::trace!(error = %e, "read_clip_content failed");
                *clipboard = None;
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(t) = linux_cli_get_text() {
            if !t.is_empty() {
                return Ok(ClipContent::Text(t));
            }
        }
    }

    Ok(ClipContent::Empty)
}

/// Probe whether we can talk to an OS clipboard (used by headless banner).
pub fn probe_clipboard_available() -> Result<()> {
    let cb = open_clipboard_handle();
    if cb.is_some() {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        // CLI tools may still work when arboard cannot open X11.
        if linux_cli_get_text().is_ok() || which_cmd("wl-copy") || which_cmd("xclip") {
            return Ok(());
        }
        log_linux_clipboard_hint();
        bail!(
            "no clipboard backend (X11/Wayland unreachable and no wl-copy/xclip). \
             Headless can still relay between peers; local paste needs a session display."
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("clipboard unavailable");
    }
}

fn persist_last_clip_text(text: &str) -> Result<()> {
    let dir = crate::config::Config::data_dir()?.join("last_clip");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("text.txt");
    std::fs::write(&path, text)?;
    tracing::info!(path = %path.display(), "saved received text to last_clip (OS clipboard failed)");
    Ok(())
}

fn persist_last_clip_image(width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let dir = crate::config::Config::data_dir()?.join("last_clip");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("image.png");
    let png = rgba_to_png(width, height, rgba)?;
    std::fs::write(&path, png)?;
    tracing::info!(path = %path.display(), "saved received image to last_clip (OS clipboard failed)");
    Ok(())
}

#[cfg(target_os = "linux")]
fn log_linux_clipboard_hint() {
    tracing::warn!(
        "Linux clipboard tip: for headless under a desktop user, try:\n  \
         export DISPLAY=:0\n  \
         # if needed: export XAUTHORITY=$HOME/.Xauthority\n  \
         # or install: sudo apt install xclip  (or wl-clipboard on Wayland)\n  \
         # pure servers without GUI cannot provide a system clipboard; peers still sync via network"
    );
}

#[cfg(target_os = "linux")]
fn which_cmd(name: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn linux_cli_set_text(text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // Prefer Wayland when advertised.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && which_cmd("wl-copy") {
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .spawn()
            .context("spawn wl-copy")?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("wl-copy stdin"))?
            .write_all(text.as_bytes())?;
        let st = child.wait()?;
        if st.success() {
            return Ok(());
        }
    }

    if which_cmd("xclip") {
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawn xclip")?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("xclip stdin"))?
            .write_all(text.as_bytes())?;
        let st = child.wait()?;
        if st.success() {
            return Ok(());
        }
    }

    if which_cmd("xsel") {
        let mut child = Command::new("xsel")
            .args(["--clipboard", "--input"])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawn xsel")?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("xsel stdin"))?
            .write_all(text.as_bytes())?;
        let st = child.wait()?;
        if st.success() {
            return Ok(());
        }
    }

    bail!("no working wl-copy/xclip/xsel")
}

#[cfg(target_os = "linux")]
fn linux_cli_get_text() -> Result<String> {
    use std::process::Command;

    if std::env::var_os("WAYLAND_DISPLAY").is_some() && which_cmd("wl-paste") {
        let out = Command::new("wl-paste")
            .arg("--no-newline")
            .output()
            .context("wl-paste")?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    if which_cmd("xclip") {
        let out = Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
            .context("xclip -o")?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    if which_cmd("xsel") {
        let out = Command::new("xsel")
            .args(["--clipboard", "--output"])
            .output()
            .context("xsel -o")?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    bail!("cli get text failed")
}

#[cfg(target_os = "linux")]
fn linux_cli_set_png(png: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if std::env::var_os("WAYLAND_DISPLAY").is_some() && which_cmd("wl-copy") {
        let mut child = Command::new("wl-copy")
            .args(["--type", "image/png"])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawn wl-copy image")?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("wl-copy stdin"))?
            .write_all(png)?;
        if child.wait()?.success() {
            return Ok(());
        }
    }
    if which_cmd("xclip") {
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/png"])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawn xclip image")?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("xclip stdin"))?
            .write_all(png)?;
        if child.wait()?.success() {
            return Ok(());
        }
    }
    bail!("no CLI image clipboard")
}

fn read_clip_content(clipboard: &mut Clipboard) -> Result<ClipContent> {
    // Prefer real file-list formats (Win CF_HDROP / Linux text/uri-list via arboard).
    // Do NOT treat plain text that happens to look like a path as a file list —
    // copying a "file name" as text must stay Text.
    let files = get_files_from_clipboard(clipboard)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.exists())
        .collect::<Vec<_>>();

    // A file-list format (CF_HDROP on Windows) is authoritative.  In particular,
    // a single .png/.jpg copied from Explorer must remain a file so it can be
    // pasted into a file manager.  Bitmap-only copies (screenshots and image
    // editors) do not provide a file list and are handled below.
    if !files.is_empty() {
        return Ok(ClipContent::Files(files));
    }

    // Bitmap / PNG formats (Win screenshot, PixPin, etc.). Retry for delayed render.
    for attempt in 0..8 {
        if let Some(img) = try_read_image_any(clipboard) {
            return Ok(img);
        }
        if attempt + 1 < 8 {
            thread::sleep(Duration::from_millis(25 * (attempt + 1) as u64));
        }
    }

    match clipboard.get_text() {
        Ok(t) if !t.is_empty() => Ok(ClipContent::Text(t)),
        Ok(_) => Ok(ClipContent::Empty),
        Err(_) => Ok(ClipContent::Empty),
    }
}

/// Try arboard + native CF_DIB / PNG clipboard formats.
fn try_read_image_any(clipboard: &mut Clipboard) -> Option<ClipContent> {
    match clipboard.get_image() {
        Ok(img) if img.width > 0 && img.height > 0 && !img.bytes.is_empty() => {
            return Some(ClipContent::Image {
                width: img.width as u32,
                height: img.height as u32,
                rgba: img.bytes.into_owned(),
            });
        }
        Err(e) => {
            tracing::trace!(error = %e, "arboard get_image failed");
        }
        _ => {}
    }
    #[cfg(windows)]
    {
        if let Ok((w, h, rgba)) = win_extra::read_image_formats() {
            return Some(ClipContent::Image {
                width: w,
                height: h,
                rgba,
            });
        }
    }
    None
}

/// Poll clipboard for text / file changes.
pub fn spawn_clipboard_watcher(
    service: Arc<ClipboardService>,
    on_change: impl Fn(ClipContent) + Send + 'static,
    shutdown: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("ohmycopy-clip-watch".into())
        .spawn(move || {
            let mut last_fp = service.get().map(|c| c.fingerprint()).unwrap_or_default();
            while !shutdown.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(400));
                let current = match service.get() {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let fp = current.fingerprint();
                if fp == last_fp {
                    continue;
                }
                last_fp = fp.clone();
                crate::audit::record(format!("clipboard_change fingerprint={fp}"));
                // Only suppress the exact content we wrote from a remote sync.
                // A broad "next change" flag used to swallow a real user copy
                // that landed before the watcher polled the sync write.
                if service.take_suppress_for(&fp) {
                    crate::audit::record(format!("clipboard_change suppressed fingerprint={fp}"));
                    continue;
                }
                if matches!(current, ClipContent::Empty) {
                    continue;
                }
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    on_change(current);
                }));
            }
        })
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "failed to spawn clipboard watcher");
            thread::spawn(|| {})
        })
}

// --- Platform: file list on clipboard ---

/// Extra Windows clipboard image formats (CF_DIB / registered PNG) that arboard
/// sometimes misses (PixPin, WeChat, etc. often omit CF_DIBV5).
#[cfg(windows)]
mod win_extra {
    use anyhow::{bail, Context, Result};
    use std::ptr;

    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(h: *mut core::ffi::c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(format: u32) -> *mut core::ffi::c_void;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalLock(h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn GlobalUnlock(h: *mut core::ffi::c_void) -> i32;
        fn GlobalSize(h: *mut core::ffi::c_void) -> usize;
    }

    pub fn read_image_formats() -> Result<(u32, u32, Vec<u8>)> {
        // Prefer registered PNG (many Electron / modern tools).
        if let Ok(data) = read_registered_format("PNG") {
            if let Ok(img) = image::load_from_memory(&data) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if w > 0 && h > 0 {
                    return Ok((w, h, rgba.into_raw()));
                }
            }
        }
        // Also try "image/png" (some apps)
        if let Ok(data) = read_registered_format("image/png") {
            if let Ok(img) = image::load_from_memory(&data) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if w > 0 && h > 0 {
                    return Ok((w, h, rgba.into_raw()));
                }
            }
        }
        // CF_DIBV5 then CF_DIB (classic bitmap clipboard).
        if let Ok(data) = read_standard_format(CF_DIBV5) {
            if let Ok(r) = dib_to_rgba(&data) {
                return Ok(r);
            }
        }
        if let Ok(data) = read_standard_format(CF_DIB) {
            if let Ok(r) = dib_to_rgba(&data) {
                return Ok(r);
            }
        }
        bail!("no readable image clipboard format")
    }

    fn read_registered_format(name: &str) -> Result<Vec<u8>> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let fmt = RegisterClipboardFormatW(wide.as_ptr());
            if fmt == 0 {
                bail!("RegisterClipboardFormat failed");
            }
            read_standard_format(fmt)
        }
    }

    fn read_standard_format(fmt: u32) -> Result<Vec<u8>> {
        unsafe {
            if IsClipboardFormatAvailable(fmt) == 0 {
                bail!("format not available");
            }
            if OpenClipboard(ptr::null_mut()) == 0 {
                bail!("OpenClipboard failed");
            }
            let h = GetClipboardData(fmt);
            if h.is_null() {
                CloseClipboard();
                bail!("GetClipboardData null");
            }
            let size = GlobalSize(h);
            if size == 0 {
                CloseClipboard();
                bail!("empty clipboard data");
            }
            let locked = GlobalLock(h);
            if locked.is_null() {
                CloseClipboard();
                bail!("GlobalLock failed");
            }
            let slice = std::slice::from_raw_parts(locked as *const u8, size);
            let data = slice.to_vec();
            GlobalUnlock(h);
            CloseClipboard();
            Ok(data)
        }
    }

    /// CF_DIB / CF_DIBV5: BITMAPINFOHEADER (or V5) + pixels (no BITMAPFILEHEADER).
    fn dib_to_rgba(dib: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
        if dib.len() < 40 {
            bail!("DIB too small");
        }
        // Prepend a fake BITMAPFILEHEADER so `image` can decode as BMP.
        let file_size = (14 + dib.len()) as u32;
        let mut bmp = Vec::with_capacity(file_size as usize);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&file_size.to_le_bytes());
        bmp.extend_from_slice(&0u16.to_le_bytes()); // reserved
        bmp.extend_from_slice(&0u16.to_le_bytes());
        // Offset to pixel data = 14 + biSize (first DWORD of DIB)
        let bi_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]);
        // biClrUsed / bit count for palette
        let bit_count = u16::from_le_bytes([dib[14], dib[15]]);
        let clr_used = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]);
        let palette_entries = if bit_count <= 8 {
            if clr_used != 0 {
                clr_used
            } else {
                1u32 << bit_count
            }
        } else if bit_count == 16 || bit_count == 32 {
            // BI_BITFIELDS masks: 3 DWORDs after header for V3 sometimes
            0
        } else {
            0
        };
        let pixel_offset = 14 + bi_size + palette_entries * 4;
        bmp.extend_from_slice(&pixel_offset.to_le_bytes());
        bmp.extend_from_slice(dib);

        let img = image::load_from_memory(&bmp)
            .or_else(|_| {
                // Fallback: decoder without file header (DIB alone)
                let dec = image::codecs::bmp::BmpDecoder::new_without_file_header(
                    std::io::Cursor::new(dib),
                )
                .context("BmpDecoder DIB")?;
                image::DynamicImage::from_decoder(dec).context("decode DIB")
            })
            .context("decode clipboard DIB as BMP")?
            .to_rgba8();
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            bail!("empty DIB image");
        }
        Ok((w, h, img.into_raw()))
    }

    // silence unused if GMEM not needed
    #[allow(dead_code)]
    const _: u32 = GMEM_MOVEABLE;
}

/// Read file paths from the OS clipboard (CF_HDROP on Windows, text/uri-list on Linux/macOS).
fn get_files_from_clipboard(clipboard: &mut Clipboard) -> Result<Vec<PathBuf>> {
    #[cfg(windows)]
    {
        // Prefer native HDROP (more reliable with Explorer); also try arboard.
        if let Ok(paths) = win_hdrop::get_files() {
            if !paths.is_empty() {
                return Ok(paths);
            }
        }
    }
    match clipboard.get().file_list() {
        Ok(paths) => Ok(paths),
        Err(e) => {
            tracing::trace!(error = %e, "clipboard file_list empty/unavailable");
            Ok(Vec::new())
        }
    }
}

fn set_files_resilient(clipboard: &mut Option<Clipboard>, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        bail!("empty file list");
    }

    #[cfg(windows)]
    {
        if let Ok(()) = win_hdrop::set_files(paths) {
            return Ok(());
        }
    }

    let mut last_err = None;
    for attempt in 0..3 {
        if let Ok(cb) = ensure_clipboard(clipboard) {
            match cb.set().file_list(paths) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("{e}"));
                    *clipboard = None;
                }
            }
        }
        thread::sleep(Duration::from_millis(30 * (attempt + 1) as u64));
    }

    #[cfg(target_os = "linux")]
    {
        if linux_cli_set_uri_list(paths).is_ok() {
            tracing::info!("clipboard file list set via xclip text/uri-list");
            return Ok(());
        }
    }

    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "could not put files on the system clipboard (install xclip on Linux if needed)"
        )
    }))
}

#[cfg(target_os = "linux")]
fn linux_cli_set_uri_list(paths: &[PathBuf]) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // text/uri-list: one file:// URI per line, CRLF, ends with empty line (RFC 2483).
    let mut body = String::new();
    for p in paths {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            std::env::current_dir().unwrap_or_default().join(p)
        };
        // Simple URI: file:///path (percent-encode spaces).
        let s = abs.to_string_lossy().replace('\\', "/");
        let uri = if s.starts_with('/') {
            format!("file://{}", s.replace(' ', "%20"))
        } else {
            format!("file:///{}", s.replace(' ', "%20"))
        };
        body.push_str(&uri);
        body.push_str("\r\n");
    }

    if which_cmd("xclip") {
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "text/uri-list"])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawn xclip uri-list")?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("xclip stdin"))?
            .write_all(body.as_bytes())?;
        if child.wait()?.success() {
            return Ok(());
        }
    }
    bail!("xclip uri-list failed")
}

/// True if `s` should be treated as a filesystem path for history re-copy
/// (not a bare relative file name like "notes.txt" which is plain text).
pub fn content_looks_like_absolute_path(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.contains('\n') {
        return false;
    }
    // Platform-native absolute (Windows drive, Unix root on Unix, etc.).
    if std::path::Path::new(t).is_absolute() {
        return true;
    }
    // Always treat Unix-style absolute paths as absolute (even when tests run on Windows).
    if t.starts_with('/') {
        return true;
    }
    // Windows drive paths (also when heuristic is used cross-platform).
    let bytes = t.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    false
}

#[cfg(test)]
mod path_heuristic_tests {
    use super::content_looks_like_absolute_path;

    #[test]
    fn bare_filename_is_not_path() {
        assert!(!content_looks_like_absolute_path("readme.md"));
        assert!(!content_looks_like_absolute_path("notes.txt"));
        assert!(!content_looks_like_absolute_path("  photo.png  "));
    }

    #[test]
    fn absolute_unix_path_is_path() {
        assert!(content_looks_like_absolute_path("/home/user/a.txt"));
        assert!(content_looks_like_absolute_path("/tmp/x"));
    }

    #[test]
    fn absolute_windows_path_is_path() {
        assert!(content_looks_like_absolute_path(r"C:\Users\a\b.txt"));
        assert!(content_looks_like_absolute_path("D:/data/file.bin"));
    }
}

#[cfg(windows)]
mod win_hdrop {
    use anyhow::{bail, Result};
    use std::path::PathBuf;
    use std::ptr;

    const CF_HDROP: u32 = 15;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[repr(C)]
    struct DropFiles {
        p_files: u32,
        pt_x: i32,
        pt_y: i32,
        f_nc: i32,
        f_wide: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(h: *mut core::ffi::c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut core::ffi::c_void;
        fn SetClipboardData(format: u32, h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut core::ffi::c_void;
        fn GlobalLock(h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn GlobalUnlock(h: *mut core::ffi::c_void) -> i32;
        fn GlobalFree(h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn DragQueryFileW(
            hdrop: *mut core::ffi::c_void,
            i_file: u32,
            lpsz_file: *mut u16,
            cch: u32,
        ) -> u32;
    }

    pub fn get_files() -> Result<Vec<PathBuf>> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 {
                return Ok(Vec::new());
            }
            if OpenClipboard(ptr::null_mut()) == 0 {
                bail!("OpenClipboard failed");
            }
            let h = GetClipboardData(CF_HDROP);
            if h.is_null() {
                CloseClipboard();
                return Ok(Vec::new());
            }
            let count = DragQueryFileW(h, 0xFFFF_FFFF, ptr::null_mut(), 0);
            let mut out = Vec::with_capacity(count as usize);
            for i in 0..count {
                let need = DragQueryFileW(h, i, ptr::null_mut(), 0) as usize;
                if need == 0 {
                    continue;
                }
                let mut buf = vec![0u16; need + 1];
                let n = DragQueryFileW(h, i, buf.as_mut_ptr(), (need + 1) as u32) as usize;
                if n == 0 {
                    continue;
                }
                let s = String::from_utf16_lossy(&buf[..n]);
                out.push(PathBuf::from(s));
            }
            CloseClipboard();
            Ok(out)
        }
    }

    pub fn set_files(paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            bail!("empty file list");
        }
        let mut last_error = None;
        // Clipboard ownership can remain briefly busy after a bitmap write
        // (notably after images copied from Explorer).  A one-shot CF_HDROP
        // write would then fail and leave the preceding bitmap on the desktop
        // clipboard.  Retry the entire native transaction before falling back
        // to arboard.
        for attempt in 0..12 {
            match set_files_once(paths) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_error = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("set CF_HDROP failed")))
    }

    fn set_files_once(paths: &[PathBuf]) -> Result<()> {
        // Build DROPFILES + double-NUL UTF-16 path list.
        let mut path_bytes: Vec<u16> = Vec::new();
        for p in paths {
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let s = abs.to_string_lossy();
            // Strip \\?\ prefix for better Explorer paste compatibility when possible.
            let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
            for c in s.encode_utf16() {
                path_bytes.push(c);
            }
            path_bytes.push(0);
        }
        path_bytes.push(0); // final double-NUL

        let header_size = std::mem::size_of::<DropFiles>();
        let total = header_size + path_bytes.len() * 2;

        unsafe {
            let hmem = GlobalAlloc(GMEM_MOVEABLE, total);
            if hmem.is_null() {
                bail!("GlobalAlloc failed");
            }
            let ptr_mem = GlobalLock(hmem);
            if ptr_mem.is_null() {
                GlobalFree(hmem);
                bail!("GlobalLock failed");
            }
            let header = DropFiles {
                p_files: header_size as u32,
                pt_x: 0,
                pt_y: 0,
                f_nc: 0,
                f_wide: 1, // Unicode
            };
            std::ptr::write(ptr_mem as *mut DropFiles, header);
            let dest = (ptr_mem as *mut u8).add(header_size) as *mut u16;
            std::ptr::copy_nonoverlapping(path_bytes.as_ptr(), dest, path_bytes.len());
            GlobalUnlock(hmem);

            if OpenClipboard(ptr::null_mut()) == 0 {
                GlobalFree(hmem);
                bail!("OpenClipboard failed");
            }
            if EmptyClipboard() == 0 {
                CloseClipboard();
                GlobalFree(hmem);
                bail!("EmptyClipboard failed");
            }
            if SetClipboardData(CF_HDROP, hmem).is_null() {
                CloseClipboard();
                GlobalFree(hmem);
                bail!("SetClipboardData CF_HDROP failed");
            }
            // Ownership of hmem transferred to clipboard.
            CloseClipboard();
        }
        Ok(())
    }
}
