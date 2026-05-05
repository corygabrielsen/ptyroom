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
pub const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

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
//
// Labels are presentation text, so they appear deliberately. Commands are the
// user operating the shell, so they get a more human cadence. Long mechanical
// payloads still stream in fast because the viewer only needs to register
// "config data", not read every color value.
pub const TYPE_FAST: Duration = ms(6);
pub const TYPE_LABEL: Duration = ms(35);
pub const TYPE_COMMAND: Duration = ms(50);

// Beats (Enter dwells).
/// Pre-Enter on bg-flip commands — viewer registers what's about to happen.
pub const PAYLOAD_PRE: Duration = ms(300);
/// Post-Enter on bg-flip commands — bg lands, viewer absorbs.
pub const PAYLOAD_SETTLE: Duration = ms(1000);
/// Post-Enter settle for every apply-by-name example.
pub const APPLY_STEP_SETTLE: Duration = ms(1000);
/// Post-Enter on the final feature's payload (the demo's climax).
pub const CLIMAX_SETTLE: Duration = ms(1500);
/// Pre-Enter on intermediate plumbing commands (mkdir, cd, eval).
pub const PLUMB_PRE: Duration = ms(250);
/// Post-Enter on intermediate plumbing commands.
pub const PLUMB_SETTLE: Duration = ms(400);
/// Pre-Enter on `clear` — "you've seen everything; clearing now" beat
/// with the typed `clear` visible on the prompt.
pub const CLEAR_REGISTER: Duration = ms(250);
/// Breath between a feature's payoff and the reset/clear coda in standalone
/// feature GIFs.
pub const STANDALONE_FEATURE_RESET_DWELL: Duration = ms(1000);

// Picker.
/// Maximum real-time wait for the picker to claim alt-screen after
/// `tint` is invoked. Used as the timeout for `arm_watch` on
/// the alt-screen-entry escape (`\e[?1049h`). Observed dev-machine
/// time is ~50ms; 500ms leaves room for Docker jitter without hiding
/// a genuinely stuck picker.
pub const PICKER_STARTUP_TIMEOUT: Duration = ms(500);
/// Cast-time visible buffer between alt-screen entry and the first
/// arrow keystroke. Real-time has already been spent by `arm_watch`
/// blocking until the picker is ready; this is the playback-only beat
/// that makes the picker feel like it "appears" instead of being
/// instantly navigated. 500ms is a comfortable register-it pause.
pub const PICKER_STARTUP_VISIBLE: Duration = ms(500);
/// Post-accept dwell after the picker returns to the shell.
pub const PICKER_DIGEST: Duration = ms(1000);
/// Dwell at overshoot before scrolling back to the target.
pub const PICKER_OVERSHOOT: Duration = ms(500);
/// Dwell on the selected target before the commit Enter.
pub const PICKER_HOLD: Duration = ms(1000);
/// Per-key cadence for picker navigation. Same speed in both
/// directions — varying it creates a barely-perceptible rhythm shift
/// when the picker scrolls back; the OVERSHOOT and HOLD beats carry
/// the "this is the chosen one" narrative weight instead.
pub const PICKER_NAV_PER_KEY: Duration = ms(50);
/// Wall-clock capture window per picker navigation key. Navigation must be
/// captured one key at a time; batching all key output into one cast event
/// makes playback jump straight from start to final selection.
pub const PICKER_NAV_CAPTURE_SETTLE: Duration = ms(20);
/// Maximum real-time wait for the picker to commit and return to the shell
/// prompt after Enter. The prompt is the important ordering gate: the picker
/// may print the selected theme name after leaving alt-screen, and the next
/// synthetic scene beat must not start until those bytes are captured.
pub const PICKER_COMMIT_TIMEOUT: Duration = ms(1000);
/// Wall-clock capture drain after picker state transitions that do not
/// currently expose a stronger content gate. Kept small because cast
/// presentation time is handled by the surrounding picker beats.
pub const PICKER_CAPTURE_SETTLE: Duration = Duration::ZERO;
/// Alt-screen-entry CSI sequence the picker emits when it claims the
/// terminal. Used as the `arm_watch` target for picker startup.
pub const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";
/// Alt-screen-exit CSI sequence the picker emits when it returns
/// control to bash. Used as the `arm_watch` target for commit.
pub const ALT_SCREEN_EXIT: &[u8] = b"\x1b[?1049l";
/// Text in the initial picker list's scroll affordance. Waiting for this
/// after alt-screen entry preserves the first visual frame before queued
/// navigation can race ahead.
pub const PICKER_READY_MARKER: &[u8] = b"more";
/// Prompt suffix emitted by the recorder rcfile's PS1. Scene helpers use
/// this as a content-aware shell-command completion gate.
pub const PROMPT_READY: &[u8] = b"\x1b[0m $ ";
/// Full prompt bytes emitted by the recorder rcfile.
pub const PROMPT: &[u8] = b"\x1b[31mt\x1b[33mi\x1b[32mn\x1b[36mt\x1b[0m $ ";
/// Screen clear emitted by the recorder rcfile and synthetic clear helper.
pub const CLEAR_SCREEN: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";

// Infrastructure.
/// Wall-time bash-echo settle at the start of every recording.
/// Visible time is zero (invisible to the GIF).
pub const BASH_SETTLE_WALL: Duration = ms(600);
/// Real-time cap for ordinary shell commands to return to the prompt.
pub const SHELL_PROMPT_TIMEOUT: Duration = ms(2000);

/// Type `text`, press Enter, dwell.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn line(
    r: &mut Recorder,
    text: &str,
    per_char: Duration,
    dwell_after: Duration,
    settle_after: Duration,
) -> anyhow::Result<()> {
    r.type_text(text, per_char)?;
    prompt_enter(r, dwell_after, "line prompt")?;
    if !settle_after.is_zero() {
        r.dwell(settle_after, ms(0))?;
    }
    Ok(())
}

/// Visual spacing — Enter on an empty prompt.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn blank(r: &mut Recorder, dwell: Duration) -> anyhow::Result<()> {
    virtual_prompt_enter(r, dwell)
}

/// Presentation-only comment/no-op line.
///
/// The shell does not need to execute comment lines for the demo to be real;
/// the following live command can run on the same underlying prompt while the
/// cast shows this explanatory line in virtual time.
///
/// # Errors
/// Any [`Recorder`] virtual output error.
pub fn note(r: &mut Recorder, text: &str, per_char: Duration) -> anyhow::Result<()> {
    r.type_presentation_text(text, per_char)?;
    virtual_prompt_enter(r, Duration::ZERO)
}

/// Presentation-only feature heading followed by one extra blank prompt line.
///
/// # Errors
/// Any [`Recorder`] virtual output error.
pub fn feature_note(r: &mut Recorder, text: &str) -> anyhow::Result<()> {
    note(r, text, TYPE_LABEL)?;
    blank(r, ms(0))
}

/// Presentation-only Enter on the prompt.
///
/// # Errors
/// Any [`Recorder`] virtual output error.
pub fn virtual_prompt_enter(r: &mut Recorder, dwell: Duration) -> anyhow::Result<()> {
    let mut output = Vec::with_capacity(2 + PROMPT.len());
    output.extend_from_slice(b"\r\n");
    output.extend_from_slice(PROMPT);
    r.push_presentation_output(output, dwell)
}

/// Presentation-only `clear` command.
///
/// # Errors
/// Any [`Recorder`] virtual output error.
pub fn virtual_clear(r: &mut Recorder, pre_enter: Duration) -> anyhow::Result<()> {
    r.type_presentation_text("clear", TYPE_COMMAND)?;
    r.advance_virtual_time(pre_enter)?;

    let mut output = Vec::with_capacity(2 + CLEAR_SCREEN.len() + PROMPT.len());
    output.extend_from_slice(b"\r\n");
    output.extend_from_slice(CLEAR_SCREEN);
    output.extend_from_slice(PROMPT);
    r.push_presentation_output(output, Duration::ZERO)
}

/// Press Enter and wait until bash has redrawn the prompt.
///
/// # Errors
/// Any [`Recorder`] IO error, or prompt timeout.
pub fn prompt_enter(r: &mut Recorder, dwell: Duration, label: &str) -> anyhow::Result<()> {
    r.send_raw_wait_for(
        Key::Enter.bytes(),
        dwell,
        PROMPT_READY,
        SHELL_PROMPT_TIMEOUT,
        label,
    )?;
    Ok(())
}

/// Wait until bash has drawn the prompt without sending input.
///
/// # Errors
/// Any [`Recorder`] IO error, or prompt timeout.
pub fn wait_for_prompt(r: &mut Recorder, dwell: Duration, label: &str) -> anyhow::Result<()> {
    r.send_raw_wait_for(&[], dwell, PROMPT_READY, SHELL_PROMPT_TIMEOUT, label)?;
    Ok(())
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
        anyhow::bail!(
            "tint -l failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let names = String::from_utf8(output.stdout)?;
    for (i, name) in names.lines().enumerate() {
        if name == theme {
            return Ok(i + 1);
        }
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
/// - Title uses label speed — it frames the demo without eating time.
/// - 100ms final settle (tight). The line is short enough that it reads
///   on the way in; a long settle here makes the demo feel like it's
///   waiting before the actual content starts. Composition adds a
///   brief blank Enter after this for visual separation.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_preamble(r: &mut Recorder) -> anyhow::Result<()> {
    line(
        r,
        "# tint — terminal theme switcher",
        TYPE_LABEL,
        ms(300),
        ms(100),
    )?;
    Ok(())
}

/// Picker feature: `tint` opens the interactive picker, overshoots the
/// target by 3 to demo navigation, scrolls back up 3 to land on the
/// target, accepts with Enter.
///
/// **Pacing decisions** (each `ms()` value below has narrative intent):
/// - "tint" command typed, then a pause *before* Enter: viewer must
///   register what command is about to run; firing Enter immediately
///   reads as magic.
/// - Down-by-(down_to_target+3): overshoot by three so the viewer sees
///   navigation behavior, not just an on-rails snap to the answer. The picker
///   opens the same way as the real CLI: cursor starts on row 0, and the scene
///   drives ordinary terminal keypresses to reach the target.
/// - 700ms pause at overshoot: register that we *can* keep going.
/// - Up-by-3 uses the same per-key cadence as the down navigation; the
///   hold beat makes the chosen row feel deliberate.
/// - 1000ms dwell on target before Enter: let the chosen theme's
///   preview settle visually before commit.
/// - 1000ms post-accept breath: after Enter commits and the picker
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
pub fn run_picker(r: &mut Recorder, down_to_target: usize) -> anyhow::Result<()> {
    feature_note(r, "# pick interactively")?;

    r.type_text("tint", TYPE_COMMAND)?;
    // Arm the watch BEFORE sending Enter. Otherwise the alt-screen-enter
    // bytes can arrive during the Enter call's settle window and be
    // consumed by the recorder before the watch is in place — the
    // watch then sits there forever and times out.
    r.send_raw_wait_for(
        Key::Enter.bytes(),
        ms(0),
        ALT_SCREEN_ENTER,
        PICKER_STARTUP_TIMEOUT,
        "picker startup",
    )?;
    // Small cast-time buffer so the picker's appearance has a frame
    // budget on playback regardless of recording-machine speed. Gate on
    // the first list frame, not just alt-screen entry, so the scroll
    // affordance is captured before burst navigation starts.
    r.send_raw_wait_for(
        &[],
        PICKER_STARTUP_VISIBLE,
        PICKER_READY_MARKER,
        PICKER_STARTUP_TIMEOUT,
        "picker first frame",
    )?;

    // Overshoot by three to demo navigation, pause, scroll back. These are
    // intentionally individual events so the GIF shows the movement.
    for _ in 0..down_to_target + 3 {
        r.key_settle(
            Key::PickerDown,
            PICKER_NAV_PER_KEY,
            PICKER_NAV_CAPTURE_SETTLE,
        )?;
    }
    r.dwell(PICKER_OVERSHOOT, PICKER_CAPTURE_SETTLE)?;
    for _ in 0..3 {
        r.key_settle(Key::PickerUp, PICKER_NAV_PER_KEY, PICKER_NAV_CAPTURE_SETTLE)?;
    }
    r.dwell(PICKER_HOLD, PICKER_CAPTURE_SETTLE)?;

    // Commit. Same arm-before-trigger pattern: arm before Enter, then wait.
    // Gate on the shell prompt instead of alt-screen exit so the selected
    // theme name and prompt cannot leak into the following synthetic reset.
    r.send_raw_wait_for(
        Key::Enter.bytes(),
        ms(0),
        PROMPT_READY,
        PICKER_COMMIT_TIMEOUT,
        "picker commit",
    )?;
    r.dwell(PICKER_DIGEST, PICKER_CAPTURE_SETTLE)?;
    Ok(())
}

/// CLI feature: apply built-in themes by name.
///
/// **Pacing:**
/// - Comment line uses label speed — readable framing, not a shortcut.
/// - Each `tint <theme>` uses command speed — it's a real command.
/// - Each apply holds for a full beat so every theme visibly lands.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_cli(r: &mut Recorder) -> anyhow::Result<()> {
    feature_note(r, "# apply by name")?;
    // Three themes: dracula (dark purple) → solarized-light (cream) →
    // monokai (classic dark with vivid accents). Three is the rule-of-
    // three rhythm — completes the "you can pick anything by name" beat
    // without dragging. Sequence dark→light→dark gives visual contrast
    // each step instead of monotonically darkening or lightening.
    for (theme, settle) in [
        ("dracula", APPLY_STEP_SETTLE),
        ("solarized-light", APPLY_STEP_SETTLE),
        ("monokai", PAYLOAD_SETTLE),
    ] {
        line(
            r,
            &format!("tint {theme}"),
            TYPE_COMMAND,
            PAYLOAD_PRE,
            settle,
        )?;
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
    feature_note(r, "# auto-apply on cd")?;
    run_cd_hook_setup(r)?;
    run_cd_hook_foo_bar(r)?;
    Ok(())
}

/// Extended cd-hook feature scene for the standalone feature GIF.
///
/// The full demo keeps the shorter two-directory version; the standalone
/// feature clip has room to breathe, so it shows a third directory to make
/// the repeating mechanism explicit.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_cd_hook_feature(r: &mut Recorder) -> anyhow::Result<()> {
    feature_note(r, "# auto-apply on cd")?;
    run_cd_hook_setup(r)?;
    run_cd_hook_foo_bar(r)?;
    line(r, "cd ..", TYPE_COMMAND, PLUMB_PRE, PLUMB_SETTLE)?;
    line(
        r,
        "mkdir baz && echo pale-green > baz/.tint",
        TYPE_COMMAND,
        PLUMB_PRE,
        PLUMB_SETTLE,
    )?;
    line(r, "cd baz", TYPE_COMMAND, PAYLOAD_PRE, PAYLOAD_SETTLE)?;
    Ok(())
}

fn run_cd_hook_setup(r: &mut Recorder) -> anyhow::Result<()> {
    line(
        r,
        "eval \"$(tint hook bash)\"",
        TYPE_COMMAND,
        PLUMB_PRE,
        PLUMB_SETTLE,
    )?;
    line(r, "cd /tmp", TYPE_COMMAND, PLUMB_PRE, PLUMB_SETTLE)?;
    Ok(())
}

fn run_cd_hook_foo_bar(r: &mut Recorder) -> anyhow::Result<()> {
    // Three-step tier progression across foo/bar/baz: deep → muted → pale.
    // Each dir picks a different hue at a different lightness tier so the
    // viewer reads "this is a system of tiered themes," not "these are
    // three random colors." Generic foo/bar names instead of theme-
    // suggestive names like skyroom/yellowroom: the latter read like a
    // magic feature ("a 'skyroom' is a thing tint understands") instead
    // of the actual mechanism (tint reads .tint from any directory).
    //
    // First dir: deep-sky-blue (deep tier, cool hue) — opens dark.
    line(
        r,
        "mkdir foo && echo deep-sky-blue > foo/.tint",
        TYPE_COMMAND,
        PLUMB_PRE,
        PLUMB_SETTLE,
    )?;
    line(r, "cd foo", TYPE_COMMAND, PAYLOAD_PRE, PAYLOAD_SETTLE)?;

    // Second dir: yellow (muted base tier, warm hue) — bright contrast.
    // Seeing the bg change *twice* makes the mechanism unmistakable;
    // one could be coincidence.
    line(r, "cd ..", TYPE_COMMAND, PLUMB_PRE, PLUMB_SETTLE)?;
    line(
        r,
        "mkdir bar && echo yellow > bar/.tint",
        TYPE_COMMAND,
        PLUMB_PRE,
        PLUMB_SETTLE,
    )?;
    line(r, "cd bar", TYPE_COMMAND, PAYLOAD_PRE, PAYLOAD_SETTLE)?;
    Ok(())
}

/// Custom-theme feature: drop a `.theme` file in the user's themes dir,
/// then apply it by name.
///
/// **Pacing:**
/// - The heredoc body (`CUSTOM_THEME_LINE`) types at payload speed —
///   it's a long color spec; full speed reads as "real config", slower
///   makes it feel laborious to write.
/// - The `EOF` and final `tint matrix` line use command speed.
/// - 1500ms settle after `tint matrix` — the climax of the demo, hold
///   a beat longer than other commands so the custom color lands.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_custom_theme(r: &mut Recorder) -> anyhow::Result<()> {
    feature_note(r, "# bring your own theme")?;
    // Smooth typing through the whole "configure a theme" sequence: the
    // viewer doesn't need to absorb each intermediate command (mkdir,
    // heredoc start, color spec, EOF); they're plumbing for the
    // payoff. The settle goes on `tint matrix` at the end.
    line(
        r,
        "mkdir -p ~/.config/tint/themes",
        TYPE_COMMAND,
        ms(0),
        ms(0),
    )?;
    r.type_text(
        "cat > ~/.config/tint/themes/matrix.theme <<EOF",
        TYPE_COMMAND,
    )?;
    r.key(Key::Enter, ms(0))?;
    r.type_text(CUSTOM_THEME_LINE, TYPE_FAST)?;
    r.key(Key::Enter, ms(0))?;
    r.type_text("EOF", TYPE_COMMAND)?;
    prompt_enter(r, ms(0), "custom theme heredoc prompt")?;

    // Apply the theme we just wrote — climax of the demo.
    line(r, "tint matrix", TYPE_COMMAND, PAYLOAD_PRE, CLIMAX_SETTLE)?;
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
    line(r, "tint reset", TYPE_COMMAND, ms(300), ms(1200))?;
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
    line(r, "clear", TYPE_COMMAND, ms(300), ms(0))?;
    Ok(())
}

/// One compact full-demo subloop: brand preamble, feature body, reset,
/// and a clear-to-blank ending.
///
/// The full reel has to stay brief, so there is no extra dwell between
/// the feature's own end-beat and the reset/clear coda.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_feature_subloop(
    r: &mut Recorder,
    feature: impl FnOnce(&mut Recorder) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    run_feature_subloop_with_reset_dwell(r, feature, Duration::ZERO)
}

/// One standalone feature GIF subloop.
///
/// Standalone feature media has room to breathe, so it holds the feature
/// payoff for one extra second before resetting and clearing.
///
/// # Errors
/// Any [`Recorder`] IO error.
pub fn run_standalone_feature_subloop(
    r: &mut Recorder,
    feature: impl FnOnce(&mut Recorder) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    run_feature_subloop_with_reset_dwell(r, feature, STANDALONE_FEATURE_RESET_DWELL)
}

fn run_feature_subloop_with_reset_dwell(
    r: &mut Recorder,
    feature: impl FnOnce(&mut Recorder) -> anyhow::Result<()>,
    reset_dwell: Duration,
) -> anyhow::Result<()> {
    note(r, "# tint — terminal theme switcher", TYPE_LABEL)?;
    blank(r, ms(0))?;
    feature(r)?;
    if !reset_dwell.is_zero() {
        r.dwell(reset_dwell, ms(0))?;
    }
    blank(r, ms(0))?;
    line(r, "tint reset", TYPE_COMMAND, ms(0), ms(0))?;
    virtual_clear(r, CLEAR_REGISTER)?;
    Ok(())
}
