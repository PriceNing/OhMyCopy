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

    /// Directory of the running executable (portable layout).
    /// Falls back to process current directory if `current_exe` is unavailable.
    pub fn config_dir() -> Result<PathBuf> {
        if let Some(dir) = exe_dir() {
            return Ok(dir);
        }
        std::env::current_dir().context("cannot resolve working directory")
    }

    /// Same as config_dir — keep history next to the exe for easy management.
    pub fn data_dir() -> Result<PathBuf> {
        Self::config_dir()
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    pub fn legacy_toml_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    pub fn clients_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("clients.json"))
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

        // Migrate from older config.toml next to exe.
        let toml_path = Self::legacy_toml_path()?;
        if toml_path.exists() {
            let text = fs::read_to_string(&toml_path)
                .with_context(|| format!("read legacy {}", toml_path.display()))?;
            let mut cfg: Config = toml::from_str(&text).context("parse config.toml")?;
            cfg.normalize();
            cfg.persist_complete()?;
            tracing::info!(
                from = %toml_path.display(),
                to = %json_path.display(),
                "migrated config.toml → config.json"
            );
            return Ok(cfg);
        }

        // Migrate from previous AppData location if present.
        if let Some(legacy_dir) = Self::legacy_appdata_dir() {
            let legacy_json = legacy_dir.join("config.json");
            let legacy_toml = legacy_dir.join("config.toml");
            if legacy_json.exists() {
                let mut cfg = Self::load(&legacy_json)?;
                cfg.persist_complete()?;
                let legacy_clients = legacy_dir.join("clients.json");
                if legacy_clients.exists() {
                    let dest = Self::clients_path()?;
                    if !dest.exists() {
                        let _ = fs::copy(&legacy_clients, &dest);
                    }
                }
                let old_hist = legacy_dir.join("history.db");
                if old_hist.exists() {
                    if let Ok(dest) = Self::data_dir().map(|d| d.join("history.db")) {
                        if !dest.exists() {
                            let _ = fs::copy(&old_hist, &dest);
                        }
                    }
                }
                tracing::info!(
                    from = %legacy_json.display(),
                    to = %json_path.display(),
                    "migrated AppData config.json → exe directory"
                );
                return Ok(cfg);
            }
            if legacy_toml.exists() {
                let text = fs::read_to_string(&legacy_toml)
                    .with_context(|| format!("read legacy {}", legacy_toml.display()))?;
                let mut cfg: Config = toml::from_str(&text).context("parse legacy config.toml")?;
                cfg.normalize();
                cfg.persist_complete()?;
                tracing::info!(
                    from = %legacy_toml.display(),
                    to = %json_path.display(),
                    "migrated AppData config.toml → exe directory config.json"
                );
                return Ok(cfg);
            }
        }

        let mut cfg = Self::default();
        cfg.persist_complete()?;
        tracing::info!(path = %json_path.display(), "created default config.json (next to exe)");
        Ok(cfg)
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

/// Directory of the running executable (portable install root).
fn exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir)
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
