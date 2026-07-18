use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const DEFAULT_TCP_PORT: u16 = 3721;
pub const DEFAULT_UDP_PORT: u16 = 3721;
pub const DEFAULT_MAX_PAYLOAD: u64 = 10 * 1024 * 1024; // 10 MiB
pub const CONFIG_VERSION: u32 = 2;

/// Historical insecure factory password — must not be used for pairing.
pub const INSECURE_DEFAULT_PASSWORD: &str = "change-me";

/// Protocol-fixed salt (v0.2 decision). Same on every node.
pub const PROTOCOL_SALT: &[u8] = b"OhMyCopy-v1-fixed-salt-lan-psk";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub config_version: u32,
    pub device_name: String,
    pub device_id: Uuid,
    pub tcp_port: u16,
    pub udp_port: u16,
    /// Shared LAN password. Never log this field.
    pub password: String,
    pub max_payload_bytes: u64,
    pub history_limit: usize,
    pub discover_interval_secs: u64,
    pub theme: String,
    pub auto_start: bool,
    pub sync_enabled: bool,
    /// When true, keep a console window for logs (and headless status).
    /// Default false — GUI double-click runs without a black console.
    #[serde(default)]
    pub console: bool,
    /// When true, start with only the tray icon (main window hidden).
    #[serde(default)]
    pub start_minimized_to_tray: bool,
    /// Legacy field (migrated into clients.json). Kept for import only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_peers: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            device_name: default_device_name(),
            device_id: Uuid::new_v4(),
            tcp_port: DEFAULT_TCP_PORT,
            udp_port: DEFAULT_UDP_PORT,
            // Fresh installs get a random password so two untouched devices cannot pair.
            password: generate_random_password(),
            max_payload_bytes: DEFAULT_MAX_PAYLOAD,
            history_limit: 200,
            discover_interval_secs: 5,
            theme: "dark_glass".into(),
            auto_start: false,
            sync_enabled: true,
            console: false,
            start_minimized_to_tray: false,
            manual_peers: Vec::new(),
        }
    }
}

impl Config {
    /// True if password is empty or the historical insecure default.
    pub fn is_insecure_default_password(password: &str) -> bool {
        let p = password.trim();
        p.is_empty() || p == INSECURE_DEFAULT_PASSWORD
    }

    pub fn has_insecure_default_password(&self) -> bool {
        Self::is_insecure_default_password(&self.password)
    }

    /// User data directory: `~/.ohmycopy` on Windows and Linux
    /// (e.g. `C:\Users\<name>\.ohmycopy`, `/home/<name>/.ohmycopy`).
    pub fn config_dir() -> Result<PathBuf> {
        let dir = home_dir()?.join(".ohmycopy");
        fs::create_dir_all(&dir)
            .with_context(|| format!("create config dir {}", dir.display()))?;
        Ok(dir)
    }

    /// Same as config_dir (history / inbox live next to config).
    pub fn data_dir() -> Result<PathBuf> {
        Self::config_dir()
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    pub fn clients_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("clients.json"))
    }

    /// Open `~/.ohmycopy` in the system file manager (Explorer / xdg-open / open).
    pub fn open_config_folder() -> Result<()> {
        let dir = Self::config_dir()?;
        open_path_in_file_manager(&dir)
            .with_context(|| format!("open folder {}", dir.display()))?;
        Ok(())
    }

    /// Old portable layout: next to the executable.
    fn legacy_exe_dir() -> Option<PathBuf> {
        exe_dir()
    }

    /// Old installs used `%APPDATA%/OhMyCopy/OhMyCopy/` (or XDG config).
    fn legacy_appdata_dir() -> Option<PathBuf> {
        ProjectDirs::from("com", "OhMyCopy", "OhMyCopy").map(|d| d.config_dir().to_path_buf())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let mut cfg: Config = if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("toml"))
            .unwrap_or(false)
        {
            toml::from_str(&text).context("parse config.toml")?
        } else {
            // Missing fields (e.g. console) filled via #[serde(default)] / Default.
            serde_json::from_str(&text).context("parse config.json")?
        };
        cfg.normalize();
        Ok(cfg)
    }

    /// Load, fill defaults for any missing fields, and rewrite config.json so disk
    /// always contains the full schema (e.g. auto-add `"start_minimized_to_tray": false`).
    pub fn load_or_create() -> Result<Self> {
        let json_path = Self::config_path()?;
        let mut cfg = if json_path.exists() {
            Self::load(&json_path)?
        } else {
            // Migrate paths handled below via existing branches...
            return Self::load_or_create_inner();
        };
        // Always re-save so newly introduced keys appear on disk.
        if let Err(e) = cfg.persist_complete() {
            tracing::warn!(error = %e, "failed to rewrite config.json with defaults");
        }
        Ok(cfg)
    }

    fn load_or_create_inner() -> Result<Self> {
        let json_path = Self::config_path()?;

        // 1) Portable layout next to exe (previous default).
        if let Some(exe) = Self::legacy_exe_dir() {
            if let Some(cfg) = Self::try_migrate_from_dir(&exe, "exe directory")? {
                return Ok(cfg);
            }
            let toml_path = exe.join("config.toml");
            if toml_path.exists() {
                let text = fs::read_to_string(&toml_path)
                    .with_context(|| format!("read legacy {}", toml_path.display()))?;
                let mut cfg: Config = toml::from_str(&text).context("parse config.toml")?;
                cfg.normalize();
                cfg.persist_complete()?;
                Self::migrate_sidecar_files(&exe);
                tracing::info!(
                    from = %toml_path.display(),
                    to = %json_path.display(),
                    "migrated exe config.toml → ~/.ohmycopy/config.json"
                );
                return Ok(cfg);
            }
        }

        // 2) Previous AppData / XDG location.
        if let Some(legacy_dir) = Self::legacy_appdata_dir() {
            if let Some(cfg) = Self::try_migrate_from_dir(&legacy_dir, "AppData/XDG")? {
                return Ok(cfg);
            }
            let legacy_toml = legacy_dir.join("config.toml");
            if legacy_toml.exists() {
                let text = fs::read_to_string(&legacy_toml)
                    .with_context(|| format!("read legacy {}", legacy_toml.display()))?;
                let mut cfg: Config = toml::from_str(&text).context("parse legacy config.toml")?;
                cfg.normalize();
                cfg.persist_complete()?;
                Self::migrate_sidecar_files(&legacy_dir);
                tracing::info!(
                    from = %legacy_toml.display(),
                    to = %json_path.display(),
                    "migrated AppData config.toml → ~/.ohmycopy/config.json"
                );
                return Ok(cfg);
            }
        }

        let mut cfg = Self::default();
        cfg.persist_complete()?;
        tracing::info!(
            path = %json_path.display(),
            "created default config.json under ~/.ohmycopy"
        );
        Ok(cfg)
    }

    /// If `dir/config.json` exists, load it into `~/.ohmycopy` and copy sidecars.
    fn try_migrate_from_dir(dir: &Path, label: &str) -> Result<Option<Self>> {
        let legacy_json = dir.join("config.json");
        if !legacy_json.exists() {
            return Ok(None);
        }
        // Avoid treating ~/.ohmycopy as a "legacy" source of itself.
        if let Ok(home_cfg) = Self::config_dir() {
            if same_dir(dir, &home_cfg) {
                return Ok(None);
            }
        }
        let mut cfg = Self::load(&legacy_json)?;
        cfg.persist_complete()?;
        Self::migrate_sidecar_files(dir);
        tracing::info!(
            from = %legacy_json.display(),
            to = %Self::config_path()?.display(),
            %label,
            "migrated config.json → ~/.ohmycopy"
        );
        Ok(Some(cfg))
    }

    /// Copy clients.json, history.db, inbox/ into ~/.ohmycopy when missing.
    fn migrate_sidecar_files(from_dir: &Path) {
        let Ok(dest_dir) = Self::config_dir() else {
            return;
        };
        for name in ["clients.json", "history.db", "history.db-wal", "history.db-shm"] {
            let src = from_dir.join(name);
            let dest = dest_dir.join(name);
            if src.is_file() && !dest.exists() {
                if let Err(e) = fs::copy(&src, &dest) {
                    tracing::warn!(error = %e, from = %src.display(), "migrate sidecar failed");
                }
            }
        }
        let src_inbox = from_dir.join("inbox");
        let dest_inbox = dest_dir.join("inbox");
        if src_inbox.is_dir() && !dest_inbox.exists() {
            if let Err(e) = copy_dir_recursive(&src_inbox, &dest_inbox) {
                tracing::warn!(error = %e, "migrate inbox failed");
            }
        }
    }


    fn normalize(&mut self) {
        if self.config_version < CONFIG_VERSION {
            self.config_version = CONFIG_VERSION;
        }
        if self.device_name.trim().is_empty() {
            self.device_name = default_device_name();
        }
        if self.password.trim().is_empty() {
            // Never fall back to the public default; generate a unique one.
            self.password = generate_random_password();
        }
        if self.tcp_port == 0 {
            self.tcp_port = DEFAULT_TCP_PORT;
        }
        if self.udp_port == 0 {
            self.udp_port = DEFAULT_UDP_PORT;
        }
        if self.max_payload_bytes == 0 {
            self.max_payload_bytes = DEFAULT_MAX_PAYLOAD;
        }
        if self.history_limit == 0 {
            self.history_limit = 200;
        }
        if self.discover_interval_secs == 0 {
            self.discover_interval_secs = 5;
        }
        if self.theme.trim().is_empty() {
            self.theme = "dark_glass".into();
        }
        // console / start_minimized_to_tray: bool defaults via serde — no extra check
    }

    /// Write full schema to disk (fills missing keys for existing installs).
    pub fn persist_complete(&mut self) -> Result<()> {
        self.normalize();
        self.save()
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir()?;
        fs::create_dir_all(&dir)?;
        let path = dir.join("config.json");
        let mut to_save = self.clone();
        to_save.manual_peers.clear();
        to_save.config_version = CONFIG_VERSION;
        let text = serde_json::to_string_pretty(&to_save).context("serialize config.json")?;
        // Only rewrite when content changed (stable field order from struct).
        let prev = fs::read_to_string(&path).unwrap_or_default();
        if prev != text {
            fs::write(&path, &text).with_context(|| format!("write {}", path.display()))?;
            tracing::info!(path = %path.display(), "config.json updated (defaults filled)");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn listen_addr(&self) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], self.tcp_port))
    }
}

/// User home directory (`USERPROFILE` / `HOME` / directories crate).
fn home_dir() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("USERPROFILE") {
        if !h.trim().is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.trim().is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    if let Some(ud) = directories::UserDirs::new() {
        return Ok(ud.home_dir().to_path_buf());
    }
    anyhow::bail!("cannot resolve user home directory")
}

/// Directory of the running executable (legacy portable layout).
fn exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let ca = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Open a folder in Explorer / xdg-open / Finder.
pub fn open_path_in_file_manager(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .context("spawn explorer")?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .context("spawn open")?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .context("spawn xdg-open")?;
        return Ok(());
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        anyhow::bail!("open folder not supported on this platform");
    }
}

/// Random shared-password for new installs (`omc-` + 16 alnum chars).
pub fn generate_random_password() -> String {
    use rand::Rng;
    const C: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    let body: String = (0..16)
        .map(|_| C[rng.gen_range(0..C.len())] as char)
        .collect();
    format!("omc-{body}")
}

fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "OhMyCopy-PC".into())
}

mod hostname {
    use std::ffi::OsString;
    pub fn get() -> std::io::Result<OsString> {
        std::env::var_os("COMPUTERNAME")
            .or_else(|| std::env::var_os("HOSTNAME"))
            .or_else(|| std::env::var_os("NAME"))
            .map(Ok)
            .unwrap_or_else(|| {
                Ok(OsString::from(
                    whoami_fallback().unwrap_or_else(|| "OhMyCopy-PC".into()),
                ))
            })
    }

    fn whoami_fallback() -> Option<String> {
        #[cfg(windows)]
        {
            std::env::var("COMPUTERNAME").ok()
        }
        #[cfg(not(windows))]
        {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("HOSTNAME").ok())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_config_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = Config::default();
        cfg.device_name = "Test".into();
        cfg.password = "secret".into();
        let text = serde_json::to_string_pretty(&cfg).unwrap();
        std::fs::write(&path, text).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.device_name, "Test");
        assert_eq!(loaded.password, "secret");
        assert_eq!(loaded.device_id, cfg.device_id);
        assert!(!loaded.start_minimized_to_tray);
    }

    #[test]
    fn insecure_default_password_detected() {
        assert!(Config::is_insecure_default_password("change-me"));
        assert!(Config::is_insecure_default_password(""));
        assert!(Config::is_insecure_default_password("  "));
        assert!(!Config::is_insecure_default_password("omc-abc"));
        let fresh = Config::default();
        assert!(!Config::is_insecure_default_password(&fresh.password));
        assert!(fresh.password.starts_with("omc-"));
    }

    #[test]
    fn config_dir_is_dot_ohmycopy_under_home() {
        let dir = Config::config_dir().unwrap();
        assert!(dir.ends_with(".ohmycopy"), "dir={}", dir.display());
        assert!(dir.is_dir());
    }

    #[test]
    fn missing_start_minimized_defaults_false() {
        let json = r#"{
            "config_version": 2,
            "device_name": "X",
            "device_id": "00000000-0000-4000-8000-000000000001",
            "tcp_port": 3721,
            "udp_port": 3721,
            "password": "p",
            "max_payload_bytes": 10485760,
            "history_limit": 200,
            "discover_interval_secs": 5,
            "theme": "dark_glass",
            "auto_start": false,
            "sync_enabled": true
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert!(!cfg.start_minimized_to_tray);
        assert!(!cfg.console);
    }
}
