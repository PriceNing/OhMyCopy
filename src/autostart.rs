//! Login / boot autostart (user-level).
//!
//! - Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `OhMyCopy`
//! - Linux: `~/.config/autostart/ohmycopy.desktop`
//! - macOS: LaunchAgents plist (best-effort)
//!
//! Windows note: never write `\\?\` extended paths into the Run key — Task Manager
//! "Open file location" and some login launches fail with that form.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(windows)]
const RUN_VALUE_NAME: &str = "OhMyCopy";
#[cfg(target_os = "linux")]
const DESKTOP_FILE_NAME: &str = "ohmycopy.desktop";

/// Enable or disable OS autostart to match `config.auto_start`.
/// When enabling, refreshes the path if the registered entry is stale / unusable.
pub fn apply(enabled: bool) -> Result<()> {
    if enabled {
        let exe = current_exe_path()?;
        if !exe.is_file() {
            anyhow::bail!(
                "current executable path is not a file: {}",
                exe.display()
            );
        }
        #[cfg(windows)]
        {
            // Skip rewrite only when Run points at this exe, path is normal, file exists.
            if windows_run_is_healthy_for(&exe) {
                return Ok(());
            }
        }
        enable()
    } else {
        if !is_registered() {
            return Ok(());
        }
        disable()
    }
}

/// Path of the running executable, suitable for OS autostart registration.
/// On Windows this **never** returns a `\\?\` extended path (breaks Run key UX).
pub fn current_exe_path() -> Result<PathBuf> {
    let p = std::env::current_exe().context("current_exe")?;
    let can = fs::canonicalize(&p).unwrap_or(p);
    Ok(strip_extended_path(can))
}

/// Remove Windows `\\?\` / `\\?\UNC\` prefixes for shell/Run compatibility.
pub fn strip_extended_path(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        let s = s.as_ref();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    let _ = s;
    path
}

fn enable() -> Result<()> {
    let exe = current_exe_path()?;
    #[cfg(windows)]
    {
        return windows_set_run(&exe, true);
    }
    #[cfg(target_os = "linux")]
    {
        return linux_set_desktop(&exe, true);
    }
    #[cfg(target_os = "macos")]
    {
        return macos_set_launch_agent(&exe, true);
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = exe;
        anyhow::bail!("autostart not supported on this platform");
    }
}

fn disable() -> Result<()> {
    #[cfg(windows)]
    {
        return windows_set_run(&PathBuf::new(), false);
    }
    #[cfg(target_os = "linux")]
    {
        return linux_set_desktop(&PathBuf::new(), false);
    }
    #[cfg(target_os = "macos")]
    {
        return macos_set_launch_agent(&PathBuf::new(), false);
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        anyhow::bail!("autostart not supported on this platform");
    }
}

/// Whether a login autostart entry currently exists (best-effort).
pub fn is_registered() -> bool {
    #[cfg(windows)]
    {
        return windows_is_registered();
    }
    #[cfg(target_os = "linux")]
    {
        return linux_desktop_path().map(|p| p.is_file()).unwrap_or(false);
    }
    #[cfg(target_os = "macos")]
    {
        return macos_plist_path().map(|p| p.is_file()).unwrap_or(false);
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

// --- Windows: HKCU Run via winreg (NO reg.exe — avoids console flash on every launch) ---
#[cfg(windows)]
fn run_key() -> Result<winreg::RegKey> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .context("open HKCU Run key")?;
    Ok(key)
}

#[cfg(windows)]
fn windows_run_value() -> Option<String> {
    let key = run_key().ok()?;
    key.get_value::<String, _>(RUN_VALUE_NAME).ok()
}

/// Normalize a Run-key command / path for comparison.
#[cfg(windows)]
fn normalize_run_path(s: &str) -> String {
    let mut t = s.trim().trim_matches('"').to_string();
    // Drop optional trailing args if ever present: "C:\path\app.exe" --foo
    if let Some(rest) = t.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            t = rest[..end].to_string();
        }
    }
    if let Some(rest) = t.strip_prefix(r"\\?\UNC\") {
        t = format!(r"\\{rest}");
    } else if let Some(rest) = t.strip_prefix(r"\\?\") {
        t = rest.to_string();
    }
    t.replace('/', "\\").to_ascii_lowercase()
}

#[cfg(windows)]
fn paths_match_run_value(value: &str, exe: &Path) -> bool {
    let e = strip_extended_path(exe.to_path_buf());
    normalize_run_path(value) == normalize_run_path(&e.to_string_lossy())
}

/// Registered value exists, points at this exe, path is not `\\?\`, and file exists.
#[cfg(windows)]
fn windows_run_is_healthy_for(exe: &Path) -> bool {
    let Some(val) = windows_run_value() else {
        return false;
    };
    // Extended prefix breaks Task Manager "Open file location" and can break Run.
    if val.contains(r"\\?\") {
        tracing::info!("autostart Run value uses extended path; will rewrite");
        return false;
    }
    let raw = val.trim().trim_matches('"');
    let p = PathBuf::from(raw);
    if !p.is_file() {
        tracing::warn!(
            registered = %raw,
            "autostart Run target missing; will rewrite"
        );
        return false;
    }
    paths_match_run_value(&val, exe)
}

#[cfg(windows)]
fn windows_set_run(exe: &Path, enable: bool) -> Result<()> {
    let key = run_key()?;
    if enable {
        // Shell Run expects a normal Win32 path, quoted, no \\?\ prefix.
        let clean = strip_extended_path(exe.to_path_buf());
        if !clean.is_file() {
            anyhow::bail!(
                "cannot register autostart; exe not found: {}",
                clean.display()
            );
        }
        let exe_s = clean.to_string_lossy();
        // Prefer start-minimized when config says so? Keep simple: just the exe;
        // app reads config.json for start_minimized_to_tray on launch.
        let cmd = format!("\"{exe_s}\"");
        key.set_value(RUN_VALUE_NAME, &cmd)
            .context("set HKCU Run value")?;
        tracing::info!(path = %clean.display(), "autostart enabled (HKCU Run API)");
    } else {
        // Not found is fine.
        match key.delete_value(RUN_VALUE_NAME) {
            Ok(()) => tracing::info!("autostart disabled (HKCU Run API)"),
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("找不到")
                    && !msg.to_ascii_lowercase().contains("not found")
                    && !msg.contains("2")
                {
                    // ERROR_FILE_NOT_FOUND is normal when already absent.
                    tracing::debug!(error = %e, "delete Run value (may already be absent)");
                }
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn windows_is_registered() -> bool {
    windows_run_value().is_some()
}

/// Read current Run command for diagnostics / UI.
#[cfg(windows)]
pub fn windows_registered_command() -> Option<String> {
    windows_run_value()
}

#[cfg(not(windows))]
pub fn windows_registered_command() -> Option<String> {
    None
}

// --- Linux: XDG autostart ---
#[cfg(target_os = "linux")]
fn linux_desktop_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("autostart")
        .join(DESKTOP_FILE_NAME))
}

#[cfg(target_os = "linux")]
fn linux_set_desktop(exe: &std::path::Path, enable: bool) -> Result<()> {
    let path = linux_desktop_path()?;
    if enable {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let exe_s = exe.to_string_lossy();
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=OhMyCopy\n\
             Comment=LAN clipboard sync\n\
             Exec=\"{exe_s}\"\n\
             Terminal=false\n\
             Categories=Utility;Network;\n\
             X-GNOME-Autostart-enabled=true\n"
        );
        fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
        tracing::info!(path = %path.display(), "autostart enabled (XDG desktop)");
    } else if path.exists() {
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        tracing::info!("autostart disabled (XDG desktop)");
    }
    Ok(())
}

// --- macOS: LaunchAgent ---
#[cfg(target_os = "macos")]
fn macos_plist_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join("com.ohmycopy.app.plist"))
}

#[cfg(target_os = "macos")]
fn macos_set_launch_agent(exe: &std::path::Path, enable: bool) -> Result<()> {
    let path = macos_plist_path()?;
    if enable {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let exe_s = exe.to_string_lossy();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.ohmycopy.app</string>
  <key>ProgramArguments</key>
  <array><string>{exe_s}</string></array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
"#
        );
        fs::write(&path, plist)?;
        tracing::info!(path = %path.display(), "autostart enabled (LaunchAgent)");
    } else if path.exists() {
        let _ = fs::remove_file(&path);
        tracing::info!("autostart disabled (LaunchAgent)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_exe_resolves() {
        let p = current_exe_path().unwrap();
        assert!(p.is_absolute() || p.exists());
        let s = p.to_string_lossy();
        assert!(
            !s.contains(r"\\?\"),
            "autostart path must not use extended prefix: {s}"
        );
    }

    #[test]
    fn strip_extended_prefix() {
        let p = strip_extended_path(PathBuf::from(r"\\?\D:\apps\ohmycopy.exe"));
        assert_eq!(p, PathBuf::from(r"D:\apps\ohmycopy.exe"));
        let p = strip_extended_path(PathBuf::from(r"\\?\UNC\server\share\a.exe"));
        assert_eq!(p, PathBuf::from(r"\\server\share\a.exe"));
        let p = strip_extended_path(PathBuf::from(r"D:\normal\ohmycopy.exe"));
        assert_eq!(p, PathBuf::from(r"D:\normal\ohmycopy.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn normalize_run_compares_quoted_and_extended() {
        let a = normalize_run_path(r#""\\?\D:\Apps\ohmycopy.exe""#);
        let b = normalize_run_path(r"D:\Apps\ohmycopy.exe");
        assert_eq!(a, b);
    }
}
