//! Scene helpers shared between concrete scene binaries.
//!
//! Each scene is a small Rust binary that uses the [`Recorder`] API to drive
//! a recording, then writes the cast.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::recorder::{Key, Recorder};

/// Custom palette emitted by `run_custom_theme`. 17 colors after the
/// `name:bg:fg:` triple — bg/fg/16 ANSI slots.
pub const CUSTOM_THEME_LINE: &str = concat!(
    "hot:#ff006e:#ffffff:",
    "#111111:#222222:#333333:#444444:#555555:#666666:#777777:#888888:",
    "#999999:#aaaaaa:#bbbbbb:#cccccc:#dddddd:#eeeeee:#f0f0f0:#ffffff",
);

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

// ─────────────── Per-feature scenes ───────────────
//
// Each `run_*` function drives one feature end-to-end through the recorder.
// They are reused by both `demo_full` (full marketing reel) and the
// per-feature scene binaries (picker, cli, cd_hook, custom_theme).

/// Picker feature: `tint` opens the interactive picker, scroll to a target
/// theme by index, accept with Enter.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_picker(r: &mut Recorder, target_idx: usize) -> anyhow::Result<()> {
    r.dwell(ms(800), ms(600))?;
    line(r, "# tint — terminal theme switcher", ms(35), ms(400), ms(1000))?;

    r.type_text("tint", ms(80))?;
    r.key(Key::Enter, ms(400))?;
    r.dwell(ms(900), ms(100))?;

    r.keys(Key::Down, ms(50), target_idx)?;
    r.dwell(ms(1000), ms(100))?;
    r.key(Key::Enter, ms(500))?;
    Ok(())
}

/// CLI feature: apply built-in themes by name.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_cli(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# apply themes by name from the command line",
         ms(24), ms(300), ms(700))?;
    for theme in ["dracula", "solarized-light"] {
        line(r, &format!("tint {theme}"), ms(35), ms(300), ms(900))?;
    }
    Ok(())
}

/// cd-hook feature: install the bash hook, then `cd` into directories whose
/// `.tint` file auto-applies a theme on entry.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_cd_hook(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# install the cd hook so .tint files auto-apply",
         ms(24), ms(300), ms(600))?;
    line(r, "eval \"$(tint hook bash)\"", ms(24), ms(300), ms(600))?;
    line(r, "cd /tmp", ms(24), ms(250), ms(300))?;

    line(r, "mkdir blueroom && echo blue > blueroom/.tint",
         ms(24), ms(250), ms(400))?;
    line(r, "cd blueroom", ms(24), ms(300), ms(900))?;

    line(r, "cd ..", ms(24), ms(250), ms(300))?;
    line(r, "mkdir roseroom && echo pale-rose > roseroom/.tint",
         ms(24), ms(250), ms(400))?;
    line(r, "cd roseroom", ms(24), ms(300), ms(900))?;
    Ok(())
}

/// Custom-theme feature: drop a `.theme` file in the user's themes dir,
/// then apply it by name.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_custom_theme(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# drop .theme files in ~/.config/tint/themes/",
         ms(24), ms(300), ms(900))?;
    line(r, "mkdir -p ~/.config/tint/themes",
         ms(24), ms(250), ms(300))?;

    r.type_text("cat > ~/.config/tint/themes/hot.theme <<EOF", ms(24))?;
    r.key(Key::Enter, ms(200))?;
    r.dwell(ms(300), ms(100))?;

    r.type_text(CUSTOM_THEME_LINE, ms(11))?;
    r.key(Key::Enter, ms(200))?;
    r.dwell(ms(200), ms(100))?;

    r.type_text("EOF", ms(24))?;
    r.key(Key::Enter, ms(300))?;
    r.dwell(ms(500), ms(100))?;

    line(r, "tint hot", ms(32), ms(300), ms(1200))?;
    Ok(())
}
