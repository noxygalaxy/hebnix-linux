//! glyph fallbacks for languages egui's built-in fonts can't draw (CJK, Thai,
//! Arabic/Hebrew glyphs, Devanagari). linux-port: asks fontconfig for an
//! installed font covering the language; if there is none the text just shows
//! boxes like before.
//!
//! note: egui has no RTL shaping, so Arabic/Hebrew would draw unjoined and
//! left-to-right even with a font. not shipped for that reason.

use std::path::PathBuf;

use eframe::egui;

/// languages the built-in fonts already cover need no fallback
fn needs_fallback(code: &str) -> bool {
    let lower = code.to_ascii_lowercase();
    let primary = lower.split('-').next().unwrap_or("");
    matches!(
        primary,
        "zh" | "ja" | "ko" | "ar" | "fa" | "ur" | "he" | "th" | "hi" | "mr" | "ne"
    )
}

/// the font file fontconfig picks for `code` (`fc-match :lang=<code>`)
fn fontconfig_match(code: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", &format!(":lang={code}")])
        .output()
        .ok()?;
    let file = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !file.is_empty()).then(|| PathBuf::from(file))
}

/// appends a fallback font for the active language behind whatever fonts are
/// already set, so a theme font stays first.
pub fn add_fallbacks(fonts: &mut egui::FontDefinitions) {
    let code = super::current();
    if !needs_fallback(&code) {
        return;
    }

    if let Some(bytes) = fontconfig_match(&code).and_then(|path| std::fs::read(path).ok()) {
        let name = format!("i18n-fallback-{code}");
        fonts
            .font_data
            .insert(name.clone(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        return;
    }
    tracing::warn!("i18n: no system font found for `{code}`, glyphs may be missing");
}
