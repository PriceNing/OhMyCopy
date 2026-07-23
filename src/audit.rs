//! Optional, append-only diagnostics for end-to-end sync investigations.
//!
//! The audit is deliberately disabled unless `OHMYCOPY_SYNC_AUDIT_PATH` is
//! configured. It is useful for smoke tests that need to distinguish a missed
//! clipboard change from a transport or clipboard-application failure.

use std::fmt::Display;
use std::fs::OpenOptions;
use std::io::Write;

pub fn record(message: impl Display) {
    let path = std::env::var("OHMYCOPY_SYNC_AUDIT_PATH").ok().or_else(|| {
        if std::env::var_os("OHMYCOPY_SYNC_AUDIT").is_some() {
            crate::config::Config::data_dir()
                .ok()
                .map(|p| p.join("sync-audit.log").to_string_lossy().into_owned())
        } else {
            None
        }
    });
    let Some(path) = path else { return };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{}", message);
}
