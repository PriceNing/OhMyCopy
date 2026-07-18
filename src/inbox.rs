//! Received file/folder payloads land under `inbox/` next to the executable.
//!
//! Layout per receive (avoids name clashes, keeps paste names clean):
//! ```text
//! inbox/
//!   20260718_153045_123/     ← timestamp receipt folder
//!     MyDoc.pdf              ← original name (what Ctrl+V pastes)
//!   20260718_153102_450/
//!     Photos/                ← original folder name
//!       a.jpg
//! ```
//!
//! Cleanup policy (runs after each write and on startup):
//!
//! - Max total size of inbox contents (default 256 MiB)
//! - Max number of **top-level receipt folders** (default 80)
//! - Max age of entries (default 7 days)
//!
//! Oldest modified receipt folders are removed first.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Default caps — enough for normal use without filling the disk.
pub const INBOX_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const INBOX_MAX_ENTRIES: usize = 80;
pub const INBOX_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Zip-bomb / path-abuse limits applied **during** extract (before cleanup_inbox).
pub const ZIP_MAX_ENTRIES: usize = 10_000;
pub const ZIP_MAX_DEPTH: usize = 32;
/// Uncompressed total written bytes (and zip-declared sizes).
pub const ZIP_MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// MIME for a directory packed as zip (receiver will extract to a folder).
pub const MIME_DIR_ZIP: &str = "application/x-ohmycopy-dir-zip";

pub fn inbox_dir() -> Result<PathBuf> {
    let d = crate::config::Config::data_dir()?.join("inbox");
    fs::create_dir_all(&d).with_context(|| format!("create inbox {}", d.display()))?;
    Ok(d)
}

/// Sanitize a single path component for Windows/Linux.
pub fn sanitize_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if r#"\/:*?"<>|"#.contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_matches('.');
    if s.is_empty() {
        "item".into()
    } else {
        s.chars().take(120).collect()
    }
}

/// Create a new receipt directory: `inbox/YYYYMMDD_HHMMSS_mmm/`.
/// Each receive gets its own folder so names never collide at the top level.
fn new_receipt_dir() -> Result<PathBuf> {
    let inbox = inbox_dir()?;
    let now = chrono::Local::now();
    let stamp = format!(
        "{}_{:03}",
        now.format("%Y%m%d_%H%M%S"),
        now.timestamp_subsec_millis() % 1000
    );
    let mut dir = inbox.join(&stamp);
    let mut n = 0u32;
    while dir.exists() {
        n += 1;
        dir = inbox.join(format!("{stamp}_{n}"));
    }
    fs::create_dir_all(&dir).with_context(|| format!("create receipt dir {}", dir.display()))?;
    Ok(dir)
}

/// Write raw bytes under a new timestamp folder:
/// `inbox/20260718_153045_123/原文件名`
/// Returns path to the **inner** file (clean name for clipboard paste).
pub fn store_file(_event_prefix: &str, file_name: &str, payload: &[u8]) -> Result<PathBuf> {
    let receipt = new_receipt_dir()?;
    let dest = receipt.join(sanitize_name(file_name));
    fs::write(&dest, payload).with_context(|| format!("write {}", dest.display()))?;
    cleanup_inbox(INBOX_MAX_TOTAL_BYTES, INBOX_MAX_ENTRIES, INBOX_MAX_AGE)?;
    Ok(dest)
}

/// Unpack a dir-zip under a new timestamp folder:
/// `inbox/20260718_153045_123/原文件夹名/...`
/// Returns path to the **inner** folder (clean name for clipboard paste).
pub fn store_folder_zip(_event_prefix: &str, folder_name: &str, zip_bytes: &[u8]) -> Result<PathBuf> {
    let receipt = new_receipt_dir()?;
    let root = receipt.join(sanitize_name(folder_name));
    fs::create_dir_all(&root)?;
    if let Err(e) = extract_zip_to_dir(zip_bytes, &root) {
        let _ = fs::remove_dir_all(&receipt);
        return Err(e);
    }
    cleanup_inbox(INBOX_MAX_TOTAL_BYTES, INBOX_MAX_ENTRIES, INBOX_MAX_AGE)?;
    Ok(root)
}

/// Pack a file or directory for network transfer.
/// Returns (wire_file_name, payload, mime).
pub fn pack_path(path: &Path, max_bytes: u64) -> Result<(String, Vec<u8>, &'static str)> {
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.is_file() {
        let size = meta.len();
        if size > max_bytes {
            anyhow::bail!(
                "文件大小 {} 超过上限 {}，可在设置中提高「大小上限」",
                size,
                max_bytes
            );
        }
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file.bin".into());
        let mime = guess_mime(&name);
        Ok((name, bytes, mime))
    } else if meta.is_dir() {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "folder".into());
        let bytes = zip_directory(path, max_bytes)?;
        // Wire name keeps .zip; display uses folder name via file_name field.
        Ok((format!("{name}.zip"), bytes, MIME_DIR_ZIP))
    } else {
        anyhow::bail!("不支持的路径类型: {}", path.display());
    }
}

fn zip_directory(dir: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    // Pre-check uncompressed total to fail fast on huge trees.
    let mut uncompressed: u64 = 0;
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            uncompressed = uncompressed.saturating_add(
                entry.metadata().map(|m| m.len()).unwrap_or(0),
            );
            // Deflate usually shrinks, but allow up to max uncompressed as soft gate.
            if uncompressed > max_bytes.saturating_mul(4) {
                anyhow::bail!(
                    "文件夹内容过大（未压缩约 {} 字节），超过可接受范围（上限 {}）",
                    uncompressed,
                    max_bytes
                );
            }
        }
    }

    let mut buf: Vec<u8> = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let base = dir;

        for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.is_empty() || rel == "." {
                continue;
            }
            if entry.file_type().is_dir() {
                let name = if rel.ends_with('/') {
                    rel
                } else {
                    format!("{rel}/")
                };
                zip.add_directory(name, opts)
                    .context("zip add_directory")?;
            } else if entry.file_type().is_file() {
                zip.start_file(rel, opts).context("zip start_file")?;
                let mut f = fs::File::open(path)?;
                let mut chunk = [0u8; 64 * 1024];
                loop {
                    let n = f.read(&mut chunk)?;
                    if n == 0 {
                        break;
                    }
                    zip.write_all(&chunk[..n])?;
                }
            }
        }
        zip.finish().context("zip finish")?;
    }
    if buf.len() as u64 > max_bytes {
        anyhow::bail!(
            "文件夹打包后大小 {} 超过上限 {}，可在设置中提高「大小上限」",
            buf.len(),
            max_bytes
        );
    }
    if buf.is_empty() {
        anyhow::bail!("文件夹为空，无法同步");
    }
    Ok(buf)
}

fn extract_zip_to_dir(zip_bytes: &[u8], dest: &Path) -> Result<()> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = ZipArchive::new(cursor).context("open zip")?;
    let n_entries = archive.len();
    if n_entries > ZIP_MAX_ENTRIES {
        bail!(
            "zip 条目过多（{n_entries} > {ZIP_MAX_ENTRIES}），已拒绝解压"
        );
    }

    let mut declared_total: u64 = 0;
    let mut written_total: u64 = 0;

    for i in 0..n_entries {
        let mut file = archive.by_index(i).context("zip index")?;
        let name = file
            .enclosed_name()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| anyhow::anyhow!("unsafe zip path"))?;

        let depth = name.components().count();
        if depth > ZIP_MAX_DEPTH {
            bail!(
                "zip 路径过深（{depth} > {ZIP_MAX_DEPTH}）: {}",
                name.display()
            );
        }

        let out_path = dest.join(&name);
        if file.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }

        // Uncompressed size declared by the archive (zip-bomb signal).
        let declared = file.size();
        declared_total = declared_total.saturating_add(declared);
        if declared_total > ZIP_MAX_UNCOMPRESSED_BYTES {
            bail!(
                "zip 声明的未压缩总量过大（{} > {}）",
                declared_total,
                ZIP_MAX_UNCOMPRESSED_BYTES
            );
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut outfile = fs::File::create(&out_path)?;
        // Copy with a hard written-byte cap (do not trust only declared size).
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            written_total = written_total.saturating_add(n as u64);
            if written_total > ZIP_MAX_UNCOMPRESSED_BYTES {
                bail!(
                    "zip 实际解压字节超限（{} > {}）",
                    written_total,
                    ZIP_MAX_UNCOMPRESSED_BYTES
                );
            }
            outfile.write_all(&buf[..n])?;
        }
    }
    Ok(())
}

fn guess_mime(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".zip") {
        "application/zip"
    } else if lower.ends_with(".txt") || lower.ends_with(".md") || lower.ends_with(".log") {
        "text/plain"
    } else if lower.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

#[derive(Debug)]
struct InboxEntry {
    path: PathBuf,
    modified: SystemTime,
    /// Recursive size for dirs, file size for files.
    size: u64,
}

fn entry_size(path: &Path) -> u64 {
    if path.is_file() {
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    } else if path.is_dir() {
        WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
            .sum()
    } else {
        0
    }
}

/// Enforce size / count / age limits. Safe to call often.
pub fn cleanup_inbox(max_total: u64, max_entries: usize, max_age: Duration) -> Result<()> {
    let inbox = match inbox_dir() {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };
    let now = SystemTime::now();
    let mut entries: Vec<InboxEntry> = fs::read_dir(&inbox)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| {
            let path = e.path();
            let modified = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            InboxEntry {
                size: entry_size(&path),
                path,
                modified,
            }
        })
        .collect();

    // Drop too-old first.
    let mut removed = 0u32;
    entries.retain(|e| {
        let age_ok = now
            .duration_since(e.modified)
            .map(|d| d <= max_age)
            .unwrap_or(true);
        if !age_ok {
            let _ = remove_entry(&e.path);
            removed += 1;
            false
        } else {
            true
        }
    });

    // Oldest first for further eviction.
    entries.sort_by_key(|e| e.modified);

    while entries.len() > max_entries {
        if let Some(e) = entries.first() {
            let _ = remove_entry(&e.path);
            removed += 1;
            entries.remove(0);
        } else {
            break;
        }
    }

    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    while total > max_total {
        if let Some(e) = entries.first() {
            total = total.saturating_sub(e.size);
            let _ = remove_entry(&e.path);
            removed += 1;
            entries.remove(0);
        } else {
            break;
        }
    }

    if removed > 0 {
        tracing::info!(removed, "inbox cleaned");
    }
    Ok(())
}

fn remove_entry(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("rm dir {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("rm file {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn zip_roundtrip_folder() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("myfolder");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::write(root.join("sub").join("b.txt"), b"world").unwrap();
        let (name, bytes, mime) = pack_path(&root, 10 * 1024 * 1024).unwrap();
        assert!(name.ends_with(".zip"));
        assert_eq!(mime, MIME_DIR_ZIP);
        assert!(!bytes.is_empty());

        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        extract_zip_to_dir(&bytes, &out).unwrap();
        assert_eq!(fs::read_to_string(out.join("a.txt")).unwrap(), "hello");
        assert_eq!(
            fs::read_to_string(out.join("sub").join("b.txt")).unwrap(),
            "world"
        );
    }
}
