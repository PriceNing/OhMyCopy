//! Load system CJK fonts so Chinese UI text is not tofu (□□□).

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontTweak};
use std::path::{Path, PathBuf};

/// Candidate system fonts (prefer single-face TTF over TTC/variable fonts).
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(windows)]
    {
        let windir = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts = windir.join("Fonts");
        // Prefer static TTF first — best compatibility with ab_glyph/egui.
        for name in [
            "simhei.ttf",      // 黑体
            "simkai.ttf",      // 楷体
            "simfang.ttf",     // 仿宋
            "msyh.ttc",        // 微软雅黑 (TTC, index 0)
            "msyhl.ttc",
            "simsun.ttc",
            "NotoSansSC-Regular.otf",
            "NotoSansSC-Regular.ttf",
            // Variable font last (may fail on some egui/ab_glyph versions)
            "NotoSansSC-VF.ttf",
        ] {
            paths.push(fonts.join(name));
        }
    }

    #[cfg(target_os = "linux")]
    {
        for p in [
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/arphic/uming.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        ] {
            paths.push(PathBuf::from(p));
        }
        // User-installed fonts
        if let Some(home) = std::env::var_os("HOME") {
            let local = PathBuf::from(home).join(".local/share/fonts");
            paths.push(local.join("NotoSansSC-Regular.otf"));
            paths.push(local.join("NotoSansCJKsc-Regular.otf"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        for p in [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ] {
            paths.push(PathBuf::from(p));
        }
    }

    paths
}

fn load_first_available() -> Option<(String, FontData)> {
    for path in candidate_paths() {
        if !path.is_file() {
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("cjk")
                    .to_string();
                let mut data = FontData::from_owned(bytes);
                // TTC collections: use first face
                if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("ttc"))
                    .unwrap_or(false)
                {
                    data.index = 0;
                }
                // Slightly denser CJK line metrics
                data = data.tweak(FontTweak {
                    scale: 1.0,
                    y_offset_factor: 0.0,
                    y_offset: 0.0,
                    ..Default::default()
                });
                tracing::info!(path = %path.display(), "loaded CJK UI font");
                return Some((name, data));
            }
            Ok(_) => tracing::warn!(path = %path.display(), "font file empty"),
            Err(e) => tracing::debug!(path = %path.display(), error = %e, "skip font"),
        }
    }
    None
}

/// Install a system Chinese font as the primary proportional/monospace face.
pub fn install_cjk_fonts(ctx: &egui::Context) {
    let Some((family_name, font_data)) = load_first_available() else {
        tracing::error!(
            "未找到中文字体，界面中文将显示为方框。请安装「黑体/微软雅黑/Noto Sans SC」后重启。"
        );
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(family_name.clone(), font_data.into());

    // Put CJK first so Han glyphs resolve; Latin still falls through to default fonts.
    if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
        prop.insert(0, family_name.clone());
    }
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        mono.insert(0, family_name);
    }

    ctx.set_fonts(fonts);
}

#[allow(dead_code)]
pub fn font_exists(path: &Path) -> bool {
    path.is_file()
}
