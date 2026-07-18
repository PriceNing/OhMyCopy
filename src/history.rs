use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct HistoryItem {
    pub event_id: String,
    pub source_id: String,
    pub kind: String,
    pub mime: String,
    /// Short line for list display.
    pub preview: String,
    /// Full text used when user clicks 复制.
    pub content: String,
    pub created_at: u64,
}

pub struct HistoryStore {
    conn: Connection,
    limit: usize,
}

impl HistoryStore {
    pub fn open(path: &Path, limit: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("open history db")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS history (
                event_id TEXT PRIMARY KEY,
                source_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                mime TEXT NOT NULL,
                preview TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_history_created ON history(created_at DESC);
            "#,
        )?;
        // Migrate: full content column (older DBs only had preview).
        let _ = conn.execute("ALTER TABLE history ADD COLUMN content TEXT", []);
        Ok(Self { conn, limit })
    }

    pub fn insert_text(
        &self,
        event_id: Uuid,
        source_id: Uuid,
        text: &str,
        created_at: u64,
    ) -> Result<()> {
        let content: String = text.chars().take(512 * 1024).collect();
        // Short single-line preview for list UI (display may ellipsize further).
        let preview = make_preview(&content, 48);
        self.conn.execute(
            "INSERT OR REPLACE INTO history(event_id, source_id, kind, mime, preview, content, created_at)
             VALUES (?1, ?2, 'text', 'text/plain; charset=utf-8', ?3, ?4, ?5)",
            params![
                event_id.to_string(),
                source_id.to_string(),
                preview,
                content,
                created_at as i64
            ],
        )?;
        self.trim()?;
        Ok(())
    }

    /// File entry: preview is short display name only; content stores local path for re-copy.
    pub fn insert_file(
        &self,
        event_id: Uuid,
        source_id: Uuid,
        file_name: &str,
        local_path: &str,
        size: u64,
        created_at: u64,
    ) -> Result<()> {
        let name = short_display_name(file_name);
        let is_folder = name.starts_with("文件夹")
            || Path::new(local_path).is_dir()
            || file_name.contains('📁');
        let name = name
            .strip_prefix("文件夹 ")
            .unwrap_or(&name)
            .to_string();
        let tag = if is_folder { "[文件夹]" } else { "[文件]" };
        let preview = make_preview(&format!("{tag} {name} ({})", format_size(size)), 56);
        self.conn.execute(
            "INSERT OR REPLACE INTO history(event_id, source_id, kind, mime, preview, content, created_at)
             VALUES (?1, ?2, 'file', 'application/octet-stream', ?3, ?4, ?5)",
            params![
                event_id.to_string(),
                source_id.to_string(),
                preview,
                local_path,
                created_at as i64
            ],
        )?;
        self.trim()?;
        Ok(())
    }

    /// Image entry: content stores local PNG path for re-copy; preview never includes full paths.
    pub fn insert_image(
        &self,
        event_id: Uuid,
        source_id: Uuid,
        label: &str,
        local_path: &str,
        size: u64,
        created_at: u64,
    ) -> Result<()> {
        let name = short_display_name(label);
        let preview = make_preview(&format!("[图片] {name} ({})", format_size(size)), 56);
        self.conn.execute(
            "INSERT OR REPLACE INTO history(event_id, source_id, kind, mime, preview, content, created_at)
             VALUES (?1, ?2, 'image', 'image/png', ?3, ?4, ?5)",
            params![
                event_id.to_string(),
                source_id.to_string(),
                preview,
                local_path,
                created_at as i64
            ],
        )?;
        self.trim()?;
        Ok(())
    }

    fn trim(&self) -> Result<()> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))?;
        if count as usize > self.limit {
            let remove = count as usize - self.limit;
            self.conn.execute(
                "DELETE FROM history WHERE event_id IN (
                    SELECT event_id FROM history ORDER BY created_at ASC LIMIT ?1
                )",
                params![remove as i64],
            )?;
        }
        Ok(())
    }

    pub fn list(&self, query: &str, limit: usize) -> Result<Vec<HistoryItem>> {
        let mut out = Vec::new();
        if query.trim().is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT event_id, source_id, kind, mime, preview, created_at,
                        COALESCE(content, preview) AS content
                 FROM history ORDER BY created_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], map_row)?;
            for r in rows {
                out.push(r?);
            }
        } else {
            let q = format!("%{}%", query.trim());
            let mut stmt = self.conn.prepare(
                "SELECT event_id, source_id, kind, mime, preview, created_at,
                        COALESCE(content, preview) AS content
                 FROM history
                 WHERE preview LIKE ?1 OR IFNULL(content, '') LIKE ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![q, limit as i64], map_row)?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    }

    pub fn clear(&self) -> Result<()> {
        self.conn.execute("DELETE FROM history", [])?;
        Ok(())
    }
}

fn format_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if n >= MB {
        format!("{:.1} MiB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KiB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

/// Never show full Windows paths in the history list.
/// Strips emoji tags and keeps only the file/folder base name.
fn short_display_name(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    // Drop common prefixes callers may have added.
    for p in ["📄 ", "🖼️ ", "📁 ", "[图片] ", "[文件] ", "[文件夹] ", "📄", "🖼️", "📁"] {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest.trim().to_string();
        }
    }
    // Absolute / long paths → basename only.
    if s.contains('\\') || s.contains('/') {
        let p = PathBuf::from(&s);
        if let Some(name) = p.file_name() {
            s = name.to_string_lossy().into_owned();
        }
    }
    // Collapse whitespace
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        "未命名".into()
    } else {
        s
    }
}

fn make_preview(text: &str, max_chars: usize) -> String {
    let one_line: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let trimmed = one_line.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        format!(
            "{}…",
            trimmed.chars().take(max_chars).collect::<String>()
        )
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryItem> {
    let preview: String = row.get(4)?;
    let created_at = row.get::<_, i64>(5)? as u64;
    let content: String = row.get(6).unwrap_or_else(|_| preview.clone());
    Ok(HistoryItem {
        event_id: row.get(0)?,
        source_id: row.get(1)?,
        kind: row.get(2)?,
        mime: row.get(3)?,
        preview,
        content,
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn history_insert_list_full_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("h.db");
        let store = HistoryStore::open(&path, 100).unwrap();
        let e = Uuid::new_v4();
        let s = Uuid::new_v4();
        let long = "hello ".repeat(50);
        store.insert_text(e, s, &long, 1).unwrap();
        let items = store.list("", 10).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].preview.contains("hello"));
        assert!(items[0].content.starts_with("hello"));
        assert!(items[0].content.len() > items[0].preview.len());
    }
}
