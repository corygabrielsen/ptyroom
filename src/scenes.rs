//! Scene helpers shared between concrete scene binaries.
//!
//! Each scene is a small Rust binary that uses the [`Recorder`] API to drive
//! a recording, then writes the cast.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::recorder::{Key, Recorder};

#[must_use] 
pub const fn ms(n: u64) -> Duration { Duration::from_millis(n) }

/// Type `text`, press Enter, dwell.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn line(
    r: &mut Recorder, text: &str, per_char: Duration,
    dwell_after: Duration, settle_after: Duration,
) -> anyhow::Result<()> {
    r.type_text(text, per_char)?;
    r.key(Key::Enter, dwell_after)?;
    if !settle_after.is_zero() { r.dwell(settle_after, ms(100))?; }
    Ok(())
}

/// Visual spacing — Enter on an empty prompt.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn blank(r: &mut Recorder, dwell: Duration) -> anyhow::Result<()> {
    r.key(Key::Enter, dwell)
}

/// Look up a built-in theme's 1-based picker idx by running `tint -l` on
/// the host. Output matches the in-container theme list because the
/// Dockerfile copies the same `$TINT_PATH` script.
///
/// # Errors
/// `tint -l` exits non-zero, output is non-UTF8, or `theme` isn't in the list.
pub fn lookup_picker_idx(tint_path: &Path, theme: &str) -> anyhow::Result<usize> {
    let output = Command::new(tint_path)
        .arg("-l")
        .env_clear()
        .env("TINT_PALETTE_DIR", "")
        .env("PATH", "/usr/bin:/bin")
        .output()?;
    if !output.status.success() {
        anyhow::bail!("tint -l failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let names = String::from_utf8(output.stdout)?;
    for (i, name) in names.lines().enumerate() {
        if name == theme { return Ok(i + 1); }
    }
    anyhow::bail!("theme not found in `tint -l`: {theme:?}")
}
