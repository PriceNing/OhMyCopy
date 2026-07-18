//! Windows console show/hide for portable GUI apps.
//!
//! Default binary is `windows_subsystem = "windows"` (no console).
//! Only allocate a console when the user opts in (`console: true` / headless).

/// Hide console as early as possible (call at the very start of `main`).
/// Safe if there is no console.
#[cfg(windows)]
pub fn hide_early() {
    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_null() {
            // Hide first — FreeConsole alone can briefly flash on some systems.
            ShowWindow(hwnd, SW_HIDE);
        }
        let _ = FreeConsole();
    }
}

/// Show or hide the process console window.
///
/// - `true`: allocate a console if needed (for logs / headless status).
/// - `false`: detach and hide console (default for GUI double-click).
#[cfg(windows)]
pub fn set_visible(show: bool) {
    unsafe {
        if show {
            let existing = GetConsoleWindow();
            if existing.is_null() {
                let _ = AllocConsole();
            }
            let hwnd = GetConsoleWindow();
            if !hwnd.is_null() {
                ShowWindow(hwnd, SW_SHOW);
            }
            // Best-effort attach CRT stdio to the console.
            let _ = freopen(c"CONOUT$".as_ptr(), c"w".as_ptr(), stdout_ptr());
            let _ = freopen(c"CONOUT$".as_ptr(), c"w".as_ptr(), stderr_ptr());
            let _ = freopen(c"CONIN$".as_ptr(), c"r".as_ptr(), stdin_ptr());
        } else {
            hide_early();
        }
    }
}

#[cfg(not(windows))]
pub fn hide_early() {}

#[cfg(not(windows))]
pub fn set_visible(_show: bool) {
    // Linux/mac: no special console window toggle.
}

#[cfg(windows)]
const SW_HIDE: i32 = 0;
#[cfg(windows)]
const SW_SHOW: i32 = 5;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn AllocConsole() -> i32;
    fn FreeConsole() -> i32;
    fn GetConsoleWindow() -> *mut core::ffi::c_void;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn ShowWindow(hwnd: *mut core::ffi::c_void, n_cmd_show: i32) -> i32;
}

// MSVC/UCRT freopen for rebinding std handles after AllocConsole.
#[cfg(windows)]
#[link(name = "ucrt")]
unsafe extern "C" {
    fn freopen(
        filename: *const core::ffi::c_char,
        mode: *const core::ffi::c_char,
        stream: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void;
    fn __acrt_iob_func(index: u32) -> *mut core::ffi::c_void;
}

#[cfg(windows)]
unsafe fn stdout_ptr() -> *mut core::ffi::c_void {
    __acrt_iob_func(1)
}
#[cfg(windows)]
unsafe fn stderr_ptr() -> *mut core::ffi::c_void {
    __acrt_iob_func(2)
}
#[cfg(windows)]
unsafe fn stdin_ptr() -> *mut core::ffi::c_void {
    __acrt_iob_func(0)
}

/// Message box when GUI has no console and startup fails.
#[cfg(windows)]
pub fn error_message_box(title: &str, text: &str) {
    use std::os::windows::ffi::OsStrExt;
    let title: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let text: Vec<u16> = std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            0x00000010, // MB_ICONERROR
        );
    }
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(
        hwnd: *mut core::ffi::c_void,
        text: *const u16,
        caption: *const u16,
        utype: u32,
    ) -> i32;
}

#[cfg(not(windows))]
pub fn error_message_box(_title: &str, _text: &str) {}
