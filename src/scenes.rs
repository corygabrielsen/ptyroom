//! Scene helpers shared between concrete scene binaries.
//!
//! Each scene is a small Rust binary that uses the [`Recorder`] API to drive
//! a recording, then writes the cast.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::recorder::{Key, Recorder};

/// Custom palette emitted by `run_custom_theme`. 17 colors after the
/// `name:bg:fg:` triple — bg/fg/16 ANSI slots. Authentic Matrix:
/// near-black bg with phosphor-green fg, and an all-green ANSI ramp so
/// the PS1's t/i/n/t letters render as Matrix-coded text instead of
/// boring grey on the dark bg.
// Classic matrix: lime fg on pure black bg. The 16 ANSI shades stay all-green
// so any colored output (PS1's t/i/n/t letters, ls colors, etc.) keeps the
// matrix aesthetic instead of clashing.
pub const CUSTOM_THEME_LINE: &str = concat!(
    "matrix:#000000:#00ff00:",
    "#000000:#008800:#00ff00:#aaff00:#005533:#00aa55:#00ff66:#88ff99:",
    "#003311:#00bb22:#33ff44:#bbff44:#006644:#00cc66:#44ff77:#ddffdd",
);

#[must_use]
pub const fn ms(n: u64) -> Duration { Duration::from_millis(n) }

// ─── Pacing knobs ─────────────────────────────────────────────────────
//
// All hand-tuned timing values for the demo composition live here as
// named constants instead of scattered `ms(…)` calls. Three axes:
//
//   1. Typing speeds   (per-char) — character cadence
//   2. Beats           (full-second order) — pre/post-Enter dwells
//   3. Picker          (specific to the picker scene's mechanics)
//
// Plus one infrastructure value (BASH_SETTLE_WALL) and one loop-seam
// constraint that intentionally stays at zero (POST_CLEAR_INTRA = 0).
//
// To tweak the demo's feel, reach for one of these by name; e.g.
// "feels rushed when bg flips" → bump PAYLOAD_SETTLE.

// Typing speeds (per character).
/// Long mechanical content (the 18-color matrix theme spec).
pub const TYPE_FAST: Duration = ms(11);
/// Plumbing commands and "# auto-apply on cd"-style headers.
pub const TYPE_NORMAL: Duration = ms(24);
/// Preambles and "# pick interactively"-style intro lines.
pub const TYPE_INTRO: Duration = ms(28);
/// Payload commands the viewer is meant to read (`tint <theme>`, `tint reset`).
pub const TYPE_PAYLOAD: Duration = ms(35);
/// `clear` — deliberately weighty before the screen wipes.
pub const TYPE_CLEAR: Duration = ms(50);
/// `tint` when invoking the picker — slow build before the reveal.
pub const TYPE_PICKER_INVOKE: Duration = ms(80);

// Beats (Enter dwells).
/// Pre-Enter on bg-flip commands — viewer registers what's about to happen.
pub const PAYLOAD_PRE: Duration = ms(300);
/// Post-Enter on bg-flip commands — bg lands, viewer absorbs.
pub const PAYLOAD_SETTLE: Duration = ms(1000);
/// Post-Enter on the final feature's payload (the demo's climax).
pub const CLIMAX_SETTLE: Duration = ms(1200);
/// Pre-Enter on intermediate plumbing commands (mkdir, cd, eval).
pub const PLUMB_PRE: Duration = ms(250);
/// Post-Enter on intermediate plumbing commands.
pub const PLUMB_SETTLE: Duration = ms(400);
/// Pre-Enter on `clear` — "you've seen everything; clearing now" beat
/// with the typed `clear` visible on the prompt.
pub const CLEAR_REGISTER: Duration = ms(250);

// Picker.
/// Real-time wait for the picker process to claim stdin. Decreasing
/// this risks `^[[B` leaking before the picker is ready.
pub const PICKER_STARTUP: Duration = ms(1600);
/// Post-accept dwell — longest because the picker did the most visual work.
pub const PICKER_DIGEST: Duration = ms(2000);
/// Dwell at overshoot before scrolling back to the target.
pub const PICKER_OVERSHOOT: Duration = ms(500);
/// Dwell on the selected target before the commit Enter.
pub const PICKER_HOLD: Duration = ms(1000);
/// Down-arrow cadence in the picker.
pub const PICKER_DOWN_PER_KEY: Duration = ms(50);
/// Up-arrow cadence — deliberately slower than down to feel decisive.
pub const PICKER_UP_PER_KEY: Duration = ms(80);
/// Post-Enter dwell on the commit keystroke (real-time the picker
/// uses to write the chosen-bg OSC and exit alt-screen). Combined
/// with PICKER_DIGEST, total post-commit time is 2.5s.
pub const PICKER_COMMIT_AFTER: Duration = ms(500);

// Infrastructure.
/// Wall-time bash-echo settle at the start of every recording.
/// Visible time is zero (invisible to the GIF).
pub const BASH_SETTLE_WALL: Duration = ms(600);

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
/// frame ("this is the tint demo") so per-act headers can be terse.
///
/// **Pacing:**
/// - Title: 28ms/char, normal speed — it's the value-prop line.
/// - 100ms final settle (tight). The line is short enough that it reads
///   on the way in; a long settle here makes the demo feel like it's
///   waiting before the actual content starts. Composition adds a
///   brief blank Enter after this for visual separation.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_preamble(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# tint — terminal theme switcher",
         ms(28), ms(300), ms(100))?;
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
/// - 2000ms post-accept breath: after Enter commits and the picker
///   collapses back to the prompt with the new bg, the chosen theme
///   needs a real digest moment — the viewer just watched ~10 seconds
///   of navigation, and the "this is what you picked" beat has to be
///   long enough to feel like a payoff. In compositions where the
///   following content arrives as pure typing rhythm (no
///   between-feature blanks), this beat IS the only digest time the
///   picker's outcome gets, so it's tuned generously. Tuned down
///   from 2500ms because 2.5s started to feel sluggish on replay.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_picker(r: &mut Recorder, target_idx: usize) -> anyhow::Result<()> {
    line(r, "# pick interactively", TYPE_INTRO, ms(0), ms(0))?;

    // Type "tint" and fire immediately — picker opening IS the beat.
    r.type_text("tint", TYPE_PICKER_INVOKE)?;
    r.key(Key::Enter, ms(0))?;
    r.dwell(PICKER_STARTUP, ms(100))?;

    // Overshoot by three to demo navigation, pause, scroll back.
    r.keys(Key::Down, PICKER_DOWN_PER_KEY, target_idx + 3)?;
    r.dwell(PICKER_OVERSHOOT, ms(100))?;
    r.keys(Key::Up, PICKER_UP_PER_KEY, 3)?;
    r.dwell(PICKER_HOLD, ms(100))?;
    r.key(Key::Enter, PICKER_COMMIT_AFTER)?;
    r.dwell(PICKER_DIGEST, ms(100))?;
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
    line(r, "# apply by name", TYPE_NORMAL, ms(0), ms(0))?;
    // Three themes: dracula (dark purple) → solarized-light (cream) →
    // monokai (classic dark with vivid accents). Three is the rule-of-
    // three rhythm — completes the "you can pick anything by name" beat
    // without dragging. Sequence dark→light→dark gives visual contrast
    // each step instead of monotonically darkening or lightening.
    for theme in ["dracula", "solarized-light", "monokai"] {
        line(r, &format!("tint {theme}"), TYPE_PAYLOAD, PAYLOAD_PRE, PAYLOAD_SETTLE)?;
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
    line(r, "# auto-apply on cd", TYPE_NORMAL, ms(0), ms(0))?;
    line(r, "eval \"$(tint hook bash)\"", TYPE_NORMAL, PLUMB_PRE, PLUMB_SETTLE)?;
    line(r, "cd /tmp", TYPE_NORMAL, PLUMB_PRE, PLUMB_SETTLE)?;

    // First dir: write a .tint, cd in — bg should change to pale-sky-blue.
    // Generic foo/bar names instead of theme-suggestive names like
    // skyroom/yellowroom: the latter read like a magic feature ("a
    // 'skyroom' is a thing tint understands") instead of the actual
    // mechanism (tint reads .tint from any directory you cd into).
    line(r, "mkdir foo && echo pale-sky-blue > foo/.tint",
         TYPE_NORMAL, PLUMB_PRE, PLUMB_SETTLE)?;
    line(r, "cd foo", TYPE_NORMAL, PAYLOAD_PRE, PAYLOAD_SETTLE)?;

    // Second dir: same pattern with a contrasting theme (warm pale-yellow
    // vs cool pale-sky-blue). Two dirs instead of one because seeing the bg
    // change *twice* makes the mechanism unmistakable; one could be
    // coincidence.
    line(r, "cd ..", TYPE_NORMAL, PLUMB_PRE, PLUMB_SETTLE)?;
    line(r, "mkdir bar && echo pale-yellow > bar/.tint",
         TYPE_NORMAL, PLUMB_PRE, PLUMB_SETTLE)?;
    line(r, "cd bar", TYPE_NORMAL, PAYLOAD_PRE, PAYLOAD_SETTLE)?;
    Ok(())
}

/// Custom-theme feature: drop a `.theme` file in the user's themes dir,
/// then apply it by name.
///
/// **Pacing:**
/// - The heredoc body (`CUSTOM_THEME_LINE`) types fast (11ms/char) —
///   it's a long color spec; full speed reads as "real config", slower
///   makes it feel laborious to write.
/// - The `EOF` and final `tint matrix` line use normal command speed
///   (24-32ms/char).
/// - 1200ms settle after `tint matrix` — the climax of the demo, hold
///   a beat longer than other commands so the custom color lands.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_custom_theme(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "# bring your own theme", TYPE_NORMAL, ms(0), ms(0))?;
    // Smooth typing through the whole "configure a theme" sequence: the
    // viewer doesn't need to absorb each intermediate command (mkdir,
    // heredoc start, color spec, EOF) — they're plumbing for the
    // payoff. The settle goes on `tint matrix` at the end.
    line(r, "mkdir -p ~/.config/tint/themes", TYPE_NORMAL, ms(0), ms(0))?;
    r.type_text("cat > ~/.config/tint/themes/matrix.theme <<EOF", TYPE_NORMAL)?;
    r.key(Key::Enter, ms(0))?;
    r.type_text(CUSTOM_THEME_LINE, TYPE_FAST)?;
    r.key(Key::Enter, ms(0))?;
    r.type_text("EOF", TYPE_NORMAL)?;
    r.key(Key::Enter, ms(0))?;

    // Apply the theme we just wrote — climax of the demo.
    line(r, "tint matrix", TYPE_PAYLOAD, PAYLOAD_PRE, CLIMAX_SETTLE)?;
    Ok(())
}

/// Reset feature: short coda after the custom theme. `tint reset`
/// returns the terminal to its default colors. Doubles as a graceful
/// loop transition — the GIF ends on default-dark, which matches the
/// loop's start state, so the wrap-around isn't jarring.
///
/// **Pacing:** kept very short (one command, no narration). The viewer
/// doesn't need framing — they see the bright matrix-green flip back to
/// neutral and understand "you can undo it" without prose.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_reset(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "tint reset", ms(35), ms(300), ms(1200))?;
    Ok(())
}

/// `clear` the screen. Reusable end-cap for looping demos: wipes the
/// accumulated output, leaving the prompt at row 1. The GIF then loops
/// from "blank prompt" → "blank prompt" so the wrap-around reads as if
/// the user themselves cleared the terminal to start the demo over.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_clear(r: &mut Recorder) -> anyhow::Result<()> {
    line(r, "clear", ms(50), ms(300), ms(0))?;
    Ok(())
}
