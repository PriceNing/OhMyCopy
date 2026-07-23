//! Persistent client list (`clients.json`).
//!
//! - Only devices the user successfully connected (password OK) are stored here.
//! - Every entry is auto-connected / auto-reconnected.
//! - UDP discovery never writes this file; failed auth never writes either.

use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const CLIENTS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClientSource {
    Discover,
    #[default]
    Manual,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEntry {
    /// Optional stable id from protocol; may be null for pure IP entries.
    #[serde(default)]
    pub device_id: Option<Uuid>,
    pub name: String,
    /// `ip:port`
    pub addr: String,
    /// Always true for entries in this file (legacy field kept for JSON compat).
    #[serde(default = "default_true")]
    pub auto_connect: bool,
    /// When true: stay connected but do not send/receive clipboard with this peer.
    #[serde(default)]
    pub ignored: bool,
    /// Unix seconds
    #[serde(default)]
    pub last_seen: u64,
    #[serde(default)]
    pub source: ClientSource,
    /// Optional note for humans editing the JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn default_true() -> bool {
    true
}

impl ClientEntry {
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.addr.parse().ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientsFile {
    pub version: u32,
    #[serde(default)]
    pub clients: Vec<ClientEntry>,
}

impl Default for ClientsFile {
    fn default() -> Self {
        Self {
            version: CLIENTS_VERSION,
            clients: Vec::new(),
        }
    }
}

impl ClientsFile {
    pub fn path() -> Result<PathBuf> {
        Config::clients_path()
    }

    pub fn load_or_create() -> Result<Self> {
        let path = Self::path()?;
        let mut file = if path.exists() {
            Self::load(&path)?
        } else {
            let file = Self::default();
            tracing::info!(path = %path.display(), "created default clients.json");
            file
        };
        file.version = CLIENTS_VERSION;
        file.clients.retain(|c| c.socket_addr().is_some());
        // All saved clients are auto-connect targets.
        for c in &mut file.clients {
            c.auto_connect = true;
        }
        file.save()?;
        Ok(file)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("read clients {}", path.display()))?;
        let mut file: ClientsFile = serde_json::from_str(&text).context("parse clients.json")?;
        if file.version < CLIENTS_VERSION {
            file.version = CLIENTS_VERSION;
        }
        file.clients.retain(|c| c.socket_addr().is_some());
        for c in &mut file.clients {
            if c.name.trim().is_empty() {
                c.name = c.addr.split(':').next().unwrap_or("client").to_string();
            }
            c.auto_connect = true;
        }
        Ok(file)
    }

    pub fn save(&self) -> Result<()> {
        let dir = Config::config_dir()?;
        fs::create_dir_all(&dir)?;
        let path = dir.join("clients.json");
        let text = serde_json::to_string_pretty(self).context("serialize clients.json")?;
        let prev = fs::read_to_string(&path).unwrap_or_default();
        if prev != text {
            fs::write(&path, &text).with_context(|| format!("write {}", path.display()))?;
            tracing::debug!(path = %path.display(), "clients.json updated");
        }
        Ok(())
    }

    /// Import legacy `manual_peers` from config into clients list.
    pub fn import_manual_peers(&mut self, peers: &[String]) -> bool {
        let mut changed = false;
        for p in peers {
            let Ok(addr) = p.parse::<SocketAddr>() else {
                continue;
            };
            if self.clients.iter().any(|c| c.addr == addr.to_string()) {
                continue;
            }
            self.clients.push(ClientEntry {
                device_id: None,
                name: format!("peer@{}", addr.ip()),
                addr: addr.to_string(),
                auto_connect: true,
                ignored: false,
                last_seen: now_secs(),
                source: ClientSource::Manual,
                note: None,
            });
            changed = true;
        }
        changed
    }

    /// Add or update after a **successful** connect (password matched).
    pub fn add_paired(
        &mut self,
        device_id: Option<Uuid>,
        name: String,
        addr: SocketAddr,
        source: ClientSource,
    ) -> bool {
        let addr_s = addr.to_string();
        if let Some(existing) = self
            .clients
            .iter_mut()
            .find(|c| (device_id.is_some() && c.device_id == device_id) || c.addr == addr_s)
        {
            existing.name = name;
            existing.addr = addr_s;
            if device_id.is_some() {
                existing.device_id = device_id;
            }
            existing.auto_connect = true;
            existing.last_seen = now_secs();
            existing.source = source;
            // Keep existing.ignored when re-pairing the same device.
            return true;
        }
        self.clients.push(ClientEntry {
            device_id,
            name,
            addr: addr_s,
            auto_connect: true,
            ignored: false,
            last_seen: now_secs(),
            source,
            note: None,
        });
        true
    }

    /// Toggle ignore: connection may stay, clipboard sync is muted both ways.
    pub fn set_ignored(&mut self, device_id: Option<Uuid>, addr: &str, ignored: bool) -> bool {
        if let Some(c) = self
            .clients
            .iter_mut()
            .find(|c| (device_id.is_some() && c.device_id == device_id) || c.addr == addr)
        {
            if c.ignored != ignored {
                c.ignored = ignored;
                return true;
            }
        }
        false
    }

    /// Remove a client by device_id and/or address.
    pub fn remove(&mut self, device_id: Option<Uuid>, addr: &str) -> bool {
        let before = self.clients.len();
        self.clients.retain(|c| {
            let id_match = device_id.is_some() && c.device_id == device_id;
            let addr_match = c.addr == addr;
            !(id_match || addr_match)
        });
        self.clients.len() != before
    }

    /// device_ids currently marked ignored (for hub mute set).
    pub fn ignored_device_ids(&self) -> Vec<Uuid> {
        self.clients
            .iter()
            .filter(|c| c.ignored)
            .filter_map(|c| c.device_id)
            .collect()
    }

    pub fn ignored_addrs(&self) -> Vec<SocketAddr> {
        self.clients
            .iter()
            .filter(|c| c.ignored)
            .filter_map(|c| c.socket_addr())
            .collect()
    }

    pub fn contains_device(&self, device_id: Uuid) -> bool {
        self.clients.iter().any(|c| c.device_id == Some(device_id))
    }

    pub fn contains_addr(&self, addr: &str) -> bool {
        self.clients.iter().any(|c| c.addr == addr)
    }

    /// All clients are auto-connect targets.
    pub fn all_connect_list(&self) -> Vec<ClientEntry> {
        self.clients
            .iter()
            .filter(|c| c.socket_addr().is_some())
            .cloned()
            .collect()
    }

    pub fn as_connect_entries(&self) -> Vec<(Option<Uuid>, SocketAddr)> {
        self.clients
            .iter()
            .filter_map(|c| c.socket_addr().map(|a| (c.device_id, a)))
            .collect()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_and_remove() {
        let mut f = ClientsFile::default();
        let id = Uuid::new_v4();
        let addr: SocketAddr = "192.168.1.10:3721".parse().unwrap();
        f.add_paired(Some(id), "A".into(), addr, ClientSource::Discover);
        assert!(f.contains_device(id));
        assert_eq!(f.all_connect_list().len(), 1);
        assert!(f.remove(Some(id), &addr.to_string()));
        assert!(!f.contains_device(id));
        assert!(f.clients.is_empty());
    }

    #[test]
    fn ignore_toggle() {
        let mut f = ClientsFile::default();
        let id = Uuid::new_v4();
        let addr: SocketAddr = "192.168.1.10:3721".parse().unwrap();
        f.add_paired(Some(id), "A".into(), addr, ClientSource::Discover);
        assert!(!f.clients[0].ignored);
        assert!(f.set_ignored(Some(id), &addr.to_string(), true));
        assert!(f.clients[0].ignored);
        assert_eq!(f.ignored_device_ids(), vec![id]);
        assert!(f.set_ignored(Some(id), &addr.to_string(), false));
        assert!(!f.clients[0].ignored);
    }
}
