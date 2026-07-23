//! System tray: hide on window close; left-click shows window; right-click menu.
//!
//! Important (eframe + Windows):
//! - Default `menu_on_left_click` is **true** → must set `with_menu_on_left_click(false)`.
//! - While the main window is hidden, eframe often **stops calling `App::update`**,
//!   so `request_repaint` alone is not enough. Show / Quit must run **inside** the
//!   tray/menu callbacks (Win32 + process exit), not only when `update` drains a queue.
//! - Sync toggle is applied immediately via a callback into the engine channel.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub const WINDOW_TITLE: &str = "OhMyCopy";

#[derive(Debug, Clone)]
pub enum TrayAction {
    ShowWindow,
    Quit,
    /// Toggle sync (UI flips current state). Also applied in the menu handler.
    ToggleSync,
}

type SyncCallback = Arc<dyn Fn(bool) + Send + Sync>;

struct TrayShared {
    pending: Mutex<Vec<TrayAction>>,
    egui_ctx: Mutex<Option<egui::Context>>,
    true_quit: Arc<AtomicBool>,
    /// Mirrors UI/engine sync flag for menu handler (no need to wait for update).
    sync_enabled: AtomicBool,
    /// Called from tray/menu thread when user toggles sync.
    on_sync: Mutex<Option<SyncCallback>>,
}

pub struct AppTray {
    _tray: TrayIcon,
    /// Keep menu items alive for the process lifetime (CheckMenuItem is !Send).
    _item_show: MenuItem,
    item_sync: CheckMenuItem,
    _item_quit: MenuItem,
    shared: Arc<TrayShared>,
}

impl AppTray {
    pub fn new(sync_enabled: bool, true_quit: Arc<AtomicBool>) -> Result<Self> {
        // GTK Menu construction can panic (not just Err). Catch so GUI still runs.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::new_inner(sync_enabled, true_quit)
        })) {
            Ok(r) => r,
            Err(_) => anyhow::bail!(
                "system tray panicked during creation (GTK/display). Continuing without tray."
            ),
        }
    }

    fn new_inner(sync_enabled: bool, true_quit: Arc<AtomicBool>) -> Result<Self> {
        // Linux tray-icon/muda uses GTK. Creating Menu/TrayIcon without gtk::init
        // panics with "GTK has not been initialized" (seen on Debian/KDE).
        #[cfg(target_os = "linux")]
        {
            ensure_linux_display()?;
            if let Err(e) = gtk::init() {
                anyhow::bail!("gtk::init failed (system tray needs a graphical session): {e}");
            }
        }

        let shared = Arc::new(TrayShared {
            pending: Mutex::new(Vec::new()),
            egui_ctx: Mutex::new(None),
            true_quit: Arc::clone(&true_quit),
            sync_enabled: AtomicBool::new(sync_enabled),
            on_sync: Mutex::new(None),
        });

        let item_show = MenuItem::with_id(
            "ohmycopy.tray.show",
            crate::i18n::t("tray.show"),
            true,
            None,
        );
        let item_sync = CheckMenuItem::with_id(
            "ohmycopy.tray.sync",
            if sync_enabled {
                crate::i18n::t("app.sync_on")
            } else {
                crate::i18n::t("app.sync_off")
            },
            true,
            sync_enabled,
            None,
        );
        let item_quit = MenuItem::with_id(
            "ohmycopy.tray.quit",
            crate::i18n::t("tray.quit"),
            true,
            None,
        );

        let menu = Menu::new();
        menu.append(&item_show).context("tray menu append show")?;
        menu.append(&item_sync).context("tray menu append sync")?;
        menu.append(&PredefinedMenuItem::separator())
            .context("tray menu separator")?;
        menu.append(&item_quit).context("tray menu append quit")?;

        let icon = crate::icon::tray_icon()
            .or_else(|e| {
                tracing::warn!(error = %e, "embedded tray icon failed, using procedural fallback");
                make_tray_icon()
            })
            .context("tray icon")?;
        // Right-click = menu; left-click = app event (show window). Default is left=menu.
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(crate::i18n::t("tray.tooltip"))
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .build()
            .context("build tray icon")?;

        {
            let shared = Arc::clone(&shared);
            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                let id = ev.id.0.as_str();
                tracing::debug!(menu_id = %id, "tray MenuEvent");
                match id {
                    "ohmycopy.tray.show" => {
                        shared.pending.lock().push(TrayAction::ShowWindow);
                        // Immediate: do not wait for eframe update.
                        win32_set_window_visible(WINDOW_TITLE, true);
                    }
                    "ohmycopy.tray.sync" => {
                        let enabled = !shared.sync_enabled.load(Ordering::SeqCst);
                        shared.sync_enabled.store(enabled, Ordering::SeqCst);
                        // CheckMenuItem is !Send — text/check updated on next UI drain.
                        if let Some(cb) = shared.on_sync.lock().as_ref() {
                            cb(enabled);
                        }
                        shared.pending.lock().push(TrayAction::ToggleSync);
                    }
                    "ohmycopy.tray.quit" => {
                        shared.true_quit.store(true, Ordering::SeqCst);
                        shared.pending.lock().push(TrayAction::Quit);
                        // Try close main window; hard-exit if event loop never wakes.
                        win32_request_close(WINDOW_TITLE);
                        std::thread::spawn(|| {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            tracing::info!("tray quit: process::exit");
                            std::process::exit(0);
                        });
                    }
                    other => {
                        tracing::debug!(%other, "unhandled tray menu id");
                    }
                }
                wake_egui(&shared);
            }));
        }

        {
            let shared = Arc::clone(&shared);
            TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
                match ev {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => {
                        tracing::debug!("tray left click → show window");
                        shared.pending.lock().push(TrayAction::ShowWindow);
                        // Immediate show — does not depend on App::update.
                        win32_set_window_visible(WINDOW_TITLE, true);
                        wake_egui(&shared);
                    }
                    _ => {}
                }
            }));
        }

        Ok(Self {
            _tray: tray,
            _item_show: item_show,
            item_sync,
            _item_quit: item_quit,
            shared,
        })
    }

    /// Engine/UI bridge: apply sync from tray without waiting for egui frames.
    pub fn set_on_sync(&self, f: SyncCallback) {
        *self.shared.on_sync.lock() = Some(f);
    }

    /// Must be called from GUI frames so menu can request_repaint when possible.
    pub fn bind_egui_ctx(&self, ctx: &egui::Context) {
        *self.shared.egui_ctx.lock() = Some(ctx.clone());
    }

    pub fn is_true_quit(&self) -> bool {
        self.shared.true_quit.load(Ordering::SeqCst)
    }

    pub fn set_true_quit(&self, v: bool) {
        self.shared.true_quit.store(v, Ordering::SeqCst);
    }

    pub fn drain_actions(&self) -> Vec<TrayAction> {
        std::mem::take(&mut *self.shared.pending.lock())
    }

    pub fn set_sync_checked(&self, enabled: bool) {
        self.shared.sync_enabled.store(enabled, Ordering::SeqCst);
        self.item_sync.set_checked(enabled);
        self.item_sync.set_text(if enabled {
            crate::i18n::t("app.sync_on")
        } else {
            crate::i18n::t("app.sync_off")
        });
    }

    /// Refresh menu labels after a language hot-reload.
    pub fn refresh_i18n_labels(&self) {
        self._item_show.set_text(crate::i18n::t("tray.show"));
        self._item_quit.set_text(crate::i18n::t("tray.quit"));
        let enabled = self.shared.sync_enabled.load(Ordering::SeqCst);
        self.item_sync.set_text(if enabled {
            crate::i18n::t("app.sync_on")
        } else {
            crate::i18n::t("app.sync_off")
        });
        // Tooltip: tray-icon may not expose set_tooltip on all versions — best-effort.
    }

    pub fn sync_enabled(&self) -> bool {
        self.shared.sync_enabled.load(Ordering::SeqCst)
    }
}

fn wake_egui(shared: &TrayShared) {
    if let Some(ctx) = shared.egui_ctx.lock().clone() {
        ctx.request_repaint();
    }
}

/// Linux tray needs a real graphical session (X11 or Wayland).
#[cfg(target_os = "linux")]
fn ensure_linux_display() -> Result<()> {
    let has_x11 = std::env::var_os("DISPLAY")
        .filter(|v| !v.is_empty())
        .is_some();
    let has_wayland = std::env::var_os("WAYLAND_DISPLAY")
        .filter(|v| !v.is_empty())
        .is_some();
    if !has_x11 && !has_wayland {
        anyhow::bail!("no DISPLAY/WAYLAND_DISPLAY; system tray skipped");
    }
    Ok(())
}

/// Pump a few GTK iterations so StatusNotifier/AppIndicator can register.
/// Safe no-op when GTK is not used (non-Linux).
#[cfg(target_os = "linux")]
pub fn poll_gtk_events() {
    if gtk::is_initialized() {
        while gtk::events_pending() {
            let _ = gtk::main_iteration_do(false);
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn poll_gtk_events() {}

fn make_tray_icon() -> Result<Icon> {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = 15.5f32;
    let cy = 15.5f32;
    let r_outer = 14.0f32;
    let r_inner = 6.0f32;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d2 = dx * dx + dy * dy;
            let i = ((y * size + x) * 4) as usize;
            if d2 <= r_outer * r_outer {
                let edge = (r_outer - d2.sqrt()).clamp(0.0, 1.5) / 1.5;
                let a = (200.0 + 55.0 * edge) as u8;
                if d2 >= r_inner * r_inner {
                    rgba[i] = 90;
                    rgba[i + 1] = 170;
                    rgba[i + 2] = 255;
                    rgba[i + 3] = a;
                } else {
                    rgba[i] = 40;
                    rgba[i + 1] = 50;
                    rgba[i + 2] = 70;
                    rgba[i + 3] = 180;
                }
            }
        }
    }
    Icon::from_rgba(rgba, size, size).context("icon from rgba")
}

/// Force show/hide by window title (eframe ViewportCommand is unreliable when hidden).
#[cfg(windows)]
pub fn win32_set_window_visible(title: &str, visible: bool) {
    if !win32_set_window_visible_quiet(title, visible) && visible {
        tracing::warn!(%title, "FindWindowW failed for tray show");
    }
}

/// Like [`win32_set_window_visible`], but does not log when the HWND is not found yet.
/// Returns true if the window was found.
#[cfg(windows)]
pub fn win32_set_window_visible_quiet(title: &str, visible: bool) -> bool {
    unsafe {
        let hwnd = find_main_hwnd(title);
        if hwnd.is_null() {
            return false;
        }
        if visible {
            // Bring back from off-screen / hidden start.
            ShowWindow(hwnd, SW_RESTORE);
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            PostMessageW(hwnd, WM_NULL, 0, 0);
        } else {
            // Hide + kick off-screen so a brief show still won't flash on the desktop.
            ShowWindow(hwnd, SW_HIDE);
            let _ = SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                -32000,
                -32000,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        true
    }
}

/// Poll and hide the main window for a short period (kills first-frame flash on Windows).
#[cfg(windows)]
pub fn spawn_startup_hide_guard(title: &'static str) {
    std::thread::Builder::new()
        .name("ohmycopy-hide-guard".into())
        .spawn(move || {
            // Also keep any console hidden (should not exist for GUI builds).
            crate::console_win::hide_early();
            // ~3s of aggressive hide while winit/eframe create the HWND.
            for i in 0..150 {
                let _ = win32_set_window_visible_quiet(title, false);
                if i % 5 == 0 {
                    crate::console_win::hide_early();
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
        .ok();
}

#[cfg(not(windows))]
pub fn win32_set_window_visible_quiet(_title: &str, _visible: bool) -> bool {
    false
}

#[cfg(not(windows))]
pub fn spawn_startup_hide_guard(_title: &'static str) {}

#[cfg(windows)]
pub fn win32_request_close(title: &str) {
    unsafe {
        let hwnd = find_main_hwnd(title);
        if hwnd.is_null() {
            tracing::warn!(%title, "FindWindowW failed for tray close");
            return;
        }
        // Show first so eframe is more likely to process close.
        ShowWindow(hwnd, SW_RESTORE);
        ShowWindow(hwnd, SW_SHOW);
        PostMessageW(hwnd, WM_CLOSE, 0, 0);
    }
}

#[cfg(windows)]
unsafe fn find_main_hwnd(title: &str) -> *mut core::ffi::c_void {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    FindWindowW(std::ptr::null(), wide.as_ptr())
}

#[cfg(not(windows))]
pub fn win32_set_window_visible(_title: &str, _visible: bool) {}

#[cfg(not(windows))]
pub fn win32_request_close(_title: &str) {}

#[cfg(windows)]
const SW_HIDE: i32 = 0;
#[cfg(windows)]
const SW_SHOW: i32 = 5;
#[cfg(windows)]
const SW_RESTORE: i32 = 9;
#[cfg(windows)]
const WM_CLOSE: u32 = 0x0010;
#[cfg(windows)]
const WM_NULL: u32 = 0x0000;
#[cfg(windows)]
const SWP_NOSIZE: u32 = 0x0001;
#[cfg(windows)]
const SWP_NOZORDER: u32 = 0x0004;
#[cfg(windows)]
const SWP_NOACTIVATE: u32 = 0x0010;

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(lp_class: *const u16, lp_window: *const u16) -> *mut core::ffi::c_void;
    fn ShowWindow(hwnd: *mut core::ffi::c_void, n_cmd_show: i32) -> i32;
    fn SetForegroundWindow(hwnd: *mut core::ffi::c_void) -> i32;
    fn SetWindowPos(
        hwnd: *mut core::ffi::c_void,
        hwnd_insert_after: *mut core::ffi::c_void,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn PostMessageW(hwnd: *mut core::ffi::c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
}
