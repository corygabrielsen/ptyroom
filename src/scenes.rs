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
//
// CONVENTION: callers are responsible for the initial bash-setup dwell
// (`r.dwell(ms(800), ms(600))?;` — 600ms settle is required so bash sets
// up echo before the first keystroke). Helpers do NOT include it
// internally so they compose cleanly when chained in demo_full (only the
// first call needs the long settle).

/// Demo preamble: the value-prop line that runs before act 1. Sets the
/// frame ("this is the tint demo") without a numbered list (an earlier
/// iteration tried enumerating "1. pick / 2. apply by name / ..." but
/// the list felt heavy; the title alone is enough to set the scene).
///
/// **Pacing:**
/// - Title: 28ms/char, normal speed — it's the value-prop line.
/// - 1200ms final settle so the viewer reads the line and registers
///   the framing before the first act starts typing.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_preamble(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# tint — 4 ways to theme your terminal:",
         ms(28), ms(300), ms(1200))?;
    Ok(())
}

/// Picker feature: `tint` opens the interactive picker, overshoots the
/// target by 3 to demo navigation, scrolls back up 3 to land on the
/// target, accepts with Enter.
///
/// **Pacing decisions** (each `ms()` value below has narrative intent):
/// - "tint" command typed, then 700ms pause *before* Enter: viewer must
///   register what command is about to run; firing Enter immediately
///   reads as magic.
/// - Down-by-(target+3): overshoot by three so the viewer sees
///   navigation behavior, not just an on-rails snap to the answer.
/// - 700ms pause at overshoot: register that we *can* keep going.
/// - Up-by-3 (slower per-key 80ms vs 50ms going down): slowing the
///   return makes the "we picked this one" feel deliberate.
/// - 1000ms dwell on target before Enter: let the chosen theme's
///   preview settle visually before commit.
/// - 1200ms post-accept breath: after Enter commits and the picker
///   collapses back to the prompt with the new bg, hold a beat
///   before the next act starts typing — otherwise the chosen theme
///   doesn't get its moment of "this is what you picked" before the
///   composition's between-act blank line and the next header
///   begin pushing fresh content.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_picker(r: &mut Recorder, target_idx: usize) -> anyhow::Result<()> {
    line(r, "# pick interactively", ms(28), ms(300), ms(700))?;

    // Type "tint", then pause so the viewer can read it before invocation.
    r.type_text("tint", ms(80))?;
    r.dwell(ms(700), ms(100))?;
    r.key(Key::Enter, ms(400))?;
    r.dwell(ms(900), ms(100))?; // picker takes ~900ms to fully render

    // Overshoot by three to demo navigation, pause, scroll back.
    r.keys(Key::Down, ms(50), target_idx + 3)?;
    r.dwell(ms(700), ms(100))?;
    r.keys(Key::Up, ms(80), 3)?;
    r.dwell(ms(1000), ms(100))?; // hold on the target so the preview registers
    r.key(Key::Enter, ms(500))?;
    r.dwell(ms(1200), ms(100))?; // breathe on the chosen theme before the next act
    Ok(())
}

/// CLI feature: apply built-in themes by name.
///
/// **Pacing:**
/// - Comment line types fast (24ms/char) — it's narration, not action.
/// - Each `tint <theme>` types slower (35ms/char) — it's a real command.
/// - 900ms settle after each command so the viewer sees the new theme
///   land before the next one fires.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_cli(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# apply by name", ms(24), ms(300), ms(700))?;
    for theme in ["dracula", "solarized-light"] {
        line(r, &format!("tint {theme}"), ms(35), ms(300), ms(900))?;
    }
    Ok(())
}

/// cd-hook feature: install the bash hook, then `cd` into directories whose
/// `.tint` file auto-applies a theme on entry.
///
/// **Pacing:**
/// - Setup commands (`eval`, `cd /tmp`, `mkdir`+`echo`) settle for 300-
///   600ms each — short, since each one is just plumbing the demo.
/// - Each `cd <theme-room>` settles for 900ms — this is the *payload*
///   moment where the theme actually changes; viewer needs to register
///   the new bg.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_cd_hook(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# auto-apply on cd", ms(24), ms(300), ms(600))?;
    line(r, "eval \"$(tint hook bash)\"", ms(24), ms(300), ms(600))?;
    line(r, "cd /tmp", ms(24), ms(250), ms(300))?;

    // First room: write a .tint, cd in — bg should change to blue.
    line(r, "mkdir blueroom && echo blue > blueroom/.tint",
         ms(24), ms(250), ms(400))?;
    line(r, "cd blueroom", ms(24), ms(300), ms(900))?;

    // Second room: same pattern with a different theme. Two rooms instead
    // of one because seeing the bg change *twice* makes the mechanism
    // unmistakable; one could be coincidence.
    line(r, "cd ..", ms(24), ms(250), ms(300))?;
    line(r, "mkdir roseroom && echo pale-rose > roseroom/.tint",
         ms(24), ms(250), ms(400))?;
    line(r, "cd roseroom", ms(24), ms(300), ms(900))?;
    Ok(())
}

/// Custom-theme feature: drop a `.theme` file in the user's themes dir,
/// then apply it by name.
///
/// **Pacing:**
/// - The heredoc body (`CUSTOM_THEME_LINE`) types fast (11ms/char) —
///   it's a long color spec; full speed reads as "real config", slower
///   makes it feel laborious to write.
/// - The `EOF` and final `tint hot` line use normal command speed
///   (24-32ms/char).
/// - 1200ms settle after `tint hot` — the climax of the demo, hold a
///   beat longer than other commands so the custom color lands clearly.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_custom_theme(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# bring your own theme", ms(24), ms(300), ms(900))?;
    line(r, "mkdir -p ~/.config/tint/themes",
         ms(24), ms(250), ms(300))?;

    // Heredoc into the themes dir. Body line types fast since it's a
    // mechanical color spec, not a thing the viewer is meant to read.
    r.type_text("cat > ~/.config/tint/themes/hot.theme <<EOF", ms(24))?;
    r.key(Key::Enter, ms(200))?;
    r.dwell(ms(300), ms(100))?;

    r.type_text(CUSTOM_THEME_LINE, ms(11))?;
    r.key(Key::Enter, ms(200))?;
    r.dwell(ms(200), ms(100))?;

    r.type_text("EOF", ms(24))?;
    r.key(Key::Enter, ms(300))?;
    r.dwell(ms(500), ms(100))?;

    // Apply the theme we just wrote. 1200ms settle for the demo finale.
    line(r, "tint hot", ms(32), ms(300), ms(1200))?;
    Ok(())
}
