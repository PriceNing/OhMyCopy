//! One-shot clipboard helper for automated smoke tests.
//!
//! ```text
//! cargo run --release --example clip_probe -- set-text "hello"
//! cargo run --release --example clip_probe -- get-text
//! cargo run --release --example clip_probe -- set-file path\to\file.bin
//! cargo run --release --example clip_probe -- set-image-png path\to\a.png
//! cargo run --release --example clip_probe -- get-kind
//! ```

use anyhow::{bail, Result};
use ohmycopy::clipboard::{png_to_rgba, rgba_to_png, ClipContent, ClipboardService};
use std::env;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

fn main() -> Result<()> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "usage: clip_probe <set-text|get-text|set-file|set-folder|set-image-png|get-kind|wait-text> ..."
        );
        std::process::exit(2);
    }
    let cmd = args.remove(0);
    let clip = ClipboardService::new()?;

    match cmd.as_str() {
        "set-text" => {
            let t = args.join(" ");
            if t.is_empty() {
                bail!("empty text");
            }
            clip.set_text_local(&t)?;
            println!("ok set-text len={}", t.len());
        }
        "get-text" => {
            let t = clip.get_text()?;
            print!("{t}");
        }
        "get-kind" => match clip.get()? {
            ClipContent::Empty => println!("empty"),
            ClipContent::Text(t) => println!("text {}", t.len()),
            ClipContent::Files(p) => {
                println!("files {}", p.len());
                for x in p {
                    println!("{}", x.display());
                }
            }
            ClipContent::Image { width, height, rgba } => {
                println!("image {}x{} rgba={}", width, height, rgba.len());
            }
        },
        "set-file" | "set-folder" => {
            let p = PathBuf::from(args.first().ok_or_else(|| anyhow::anyhow!("need path"))?);
            if !p.exists() {
                bail!("path not found: {}", p.display());
            }
            if cmd == "set-file" && !p.is_file() {
                bail!("not a file: {}", p.display());
            }
            if cmd == "set-folder" && !p.is_dir() {
                bail!("not a directory: {}", p.display());
            }
            // CF_HDROP for file or folder. Use from_sync then clear suppress so only
            // OhMyCopy (other process) observes the OS clipboard change.
            let abs = p.canonicalize()?;
            clip.set_files_from_sync(&[abs.clone()])?;
            let _ = clip.take_suppress();
            println!(
                "ok {} {}",
                if p.is_dir() { "set-folder" } else { "set-file" },
                abs.display()
            );
        }
        "set-image-png" => {
            let p = PathBuf::from(args.first().ok_or_else(|| anyhow::anyhow!("need png path"))?);
            let bytes = std::fs::read(&p)?;
            let (w, h, rgba) = png_to_rgba(&bytes)?;
            clip.set_image_local(w, h, rgba)?;
            println!("ok set-image-png {}x{}", w, h);
        }
        "make-png" => {
            // write a tiny red png to path
            let p = PathBuf::from(args.first().ok_or_else(|| anyhow::anyhow!("need out path"))?);
            let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255];
            let png = rgba_to_png(2, 2, &rgba)?;
            std::fs::write(&p, png)?;
            println!("ok make-png {}", p.display());
        }
        "wait-text" => {
            // wait-text <substr> <timeout_secs>
            let sub = args.first().cloned().unwrap_or_default();
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15);
            let deadline = std::time::Instant::now() + Duration::from_secs(secs);
            loop {
                if let Ok(t) = clip.get_text() {
                    if t.contains(&sub) {
                        println!("ok wait-text matched");
                        println!("{t}");
                        return Ok(());
                    }
                }
                if std::time::Instant::now() >= deadline {
                    bail!("timeout waiting for text containing {sub:?}");
                }
                thread::sleep(Duration::from_millis(300));
            }
        }
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
