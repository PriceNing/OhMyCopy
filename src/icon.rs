//! Shared app icons: window (eframe) + tray (tray-icon).
//!
//! Assets are embedded at compile time from `assets/`.

use anyhow::{Context, Result};

/// Preferred window icon (256×256 RGBA PNG).
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
/// Compact tray icon (32×32 RGBA PNG), high-contrast for taskbar.
pub const TRAY_PNG: &[u8] = include_bytes!("../assets/tray.png");

/// Decode PNG → (width, height, RGBA8).
pub fn load_rgba(png: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(png)
        .context("decode icon png")?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok((w, h, img.into_raw()))
}

/// egui / eframe viewport icon.
pub fn egui_icon_data() -> Result<egui::IconData> {
    let (width, height, rgba) = load_rgba(ICON_PNG)?;
    Ok(egui::IconData {
        rgba,
        width,
        height,
    })
}

/// tray-icon crate icon (uses 32×32 tray asset).
pub fn tray_icon() -> Result<tray_icon::Icon> {
    let (w, h, rgba) = load_rgba(TRAY_PNG)?;
    tray_icon::Icon::from_rgba(rgba, w, h).context("tray Icon::from_rgba")
}
