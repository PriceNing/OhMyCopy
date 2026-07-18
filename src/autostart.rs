//! Login / boot autostart (user-level).
//!
//! - Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `OhMyCopy`
//! - Linux: `~/.config/autostart/ohmycopy.desktop`
//! - macOS: LaunchAgents plist (best-effort)

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

#[cfg(windows)]
const RUN_VALUE_NAME: &str = "OhMyCopy";
#[cfg(target_os = "linux")]
const DESKTOP_FILE_NAME: &str = "ohmycopy.desktop";

/// Enable or disable OS autostart to match `config.auto_start`.
/// No-ops when already in the desired state (avoids needless work / side effects).
pub fn apply(enabled: bool) -> Result<()> {
    if enabled {
        let exe = current_exe_path()?;
        if windows_already_enabled_for(&exe) {
            return Ok(());
        }
        enable()
    } else {
        if !is_registered() {
            return Ok(());
        }
        disable()
    }
}

/// True if autostart already points at this exe (Windows); other OS: any registration.
fn windows_already_enabled_for(exe: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        return windows_run_value()
            .map(|v| paths_match_run_value(&v, exe))
            .unwrap_or(false);
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        is_registered()
    }
}

/// Path of the running executable (canonical when possible).
pub fn current_exe_path() -> Result<PathBuf> {
    let p = std::env::current_exe().context("current_exe")?;
    Ok(fs::canonicalize(&p).unwrap_or(p))
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

#[cfg(windows)]
fn paths_match_run_value(value: &str, exe: &std::path::Path) -> bool {
    let v = value.trim().trim_matches('"');
    let e = exe.to_string_lossy();
    // Compare case-insensitive; tolerate \\?\ prefix differences.
    let norm = |s: &str| {
        s.trim()
            .trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    norm(v) == norm(&e)
}

#[cfg(windows)]
fn windows_set_run(exe: &std::path::Path, enable: bool) -> Result<()> {
    let key = run_key()?;
    if enable {
        let exe_s = exe.to_string_lossy();
        let cmd = format!("\"{exe_s}\"");
        key.set_value(RUN_VALUE_NAME, &cmd)
            .context("set HKCU Run value")?;
        tracing::info!(path = %exe.display(), "autostart enabled (HKCU Run API)");
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
    }
}
