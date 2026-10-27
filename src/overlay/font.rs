//! Finding and loading the tag font.
//!
//! `fc-match` (fontconfig) turns a pattern like `"Inter:bold"` into a font
//! file, honouring the user's own fontconfig aliases and substitutions, the
//! same way every other app on the desktop finds its fonts. `fontdue` then
//! rasterizes that file: tags are a few ASCII letters, so no text shaping is
//! needed.

use std::process::Command;

use anyhow::{Context as _, Result, bail};

/// Loads the font `pattern` resolves to, at `size` px.
pub fn load(pattern: &str, size: f32) -> Result<fontdue::Font> {
    let output = Command::new("fc-match")
        .args(["--format", "%{file}\n%{index}", pattern])
        .output()
        .context("running fc-match (fontconfig) to find the [hints] font")?;
    if !output.status.success() {
        bail!(
            "fc-match {pattern:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let reply = String::from_utf8(output.stdout).context("fc-match printed a non-UTF-8 path")?;
    let (path, index) = reply.split_once('\n').unwrap_or((reply.as_str(), "0"));
    let bytes = std::fs::read(path).with_context(|| format!("reading font {path}"))?;
    let settings = fontdue::FontSettings {
        // A .ttc file holds several fonts; fontconfig says which one.
        collection_index: index.trim().parse().unwrap_or(0),
        // Rasterizing is tuned for this size; other sizes still work.
        scale: size,
        ..fontdue::FontSettings::default()
    };
    // fontdue's error is a `&str` without `std::error::Error`, so it's
    // turned into one by hand.
    fontdue::Font::from_bytes(bytes, settings).map_err(|e| anyhow::anyhow!("font {path}: {e}"))
}
