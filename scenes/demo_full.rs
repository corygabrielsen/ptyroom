//! Full 4-feature marketing demo, restructured into per-feature subloops.
//!
//! Each feature gets its own self-contained mini-demo:
//!   preamble → feature → reset → clear → 500ms breath
//!
//! The fourth subloop's trailing 500ms breath *is* the GIF loop tail —
//! no special-cased ending. Because every clear-to-next-preamble seam
//! has identical timing and identical post-clear terminal state
//! (blank + PS1 + cursor at row 1), the loop wrap is visually
//! indistinguishable from any inter-subloop transition. Marketing
//! consequence: a viewer can't lock onto where the loop "started" and
//! is more likely to watch additional cycles to see new content.
//!
//! Side benefit: each subloop is short, so the recording's row count
//! drops from the 36 the stacked-acts version needed down to ~20.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{
    CLEAR_REGISTER, TYPE_NORMAL, blank, line, lookup_picker_idx, ms, note, run_cd_hook, run_cli,
    run_custom_theme, run_picker, virtual_clear, wait_for_prompt,
};

/// Theme the picker lands on. Cool/blue register reads better as the
/// reveal than a warm/orange theme, which can look default-terminal-ish.
const PICKER_TARGET: &str = "dark-azure";
/// Start near the target, not on it. This preserves the fast path while
/// still making the picker visibly navigate to the chosen theme.
const PICKER_START: &str = "dark-sky-blue";
const PICKER_DEFAULT_DOWN_TO_TARGET: usize = 1;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/demo_full.cast")]
    cast: PathBuf,
    /// Record only one subloop (0=cli, 1=picker, 2=cd-hook, 3=custom-theme).
    /// Used by the parallel-record flow: spawn 4 copies of this binary
    /// concurrently with --subloop-only 0..3 each writing its own cast,
    /// then stitch the resulting casts. Each subloop is self-contained
    /// (ends in `clear`) so per-subloop casts splice cleanly.
    #[arg(long)]
    subloop_only: Option<usize>,
    /// Resolve the picker target by running `tint -l` instead of using the
    /// baked marketing-demo index. Useful after palette-list edits.
    #[arg(long)]
    lookup_picker_target: bool,
}

/// One subloop: framing + feature, then pure-typing wrap-up.
///
/// The viewer is going to see this preamble four times per loop — any
/// filler beat between feature's own end-beat and the next preamble's
/// first character will read as dead air and disengage them. So once
/// the feature finishes its internal money-shot dwell, we type
/// `tint reset` → `clear` straight into the next preamble's `# tint —`
/// with zero added dwells anywhere.
///
/// Loop-seam invariant: every clear → next-preamble transition has the
/// same (zero) post-clear pause. The fourth subloop's clear → loop wrap
/// → frame 0 → first preamble char is timed identically to inner
/// inter-subloop transitions, so the wrap is indistinguishable.
fn run_subloop(
    r: &mut Recorder,
    feature: impl FnOnce(&mut Recorder) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Pure typing rhythm: preamble → feature → reset → clear, with zero
    // dwells between any of them. Each transition gets a single empty
    // Enter for a blank line of visual separation (no dwell, just a
    // newline byte). Inlining the typing here (instead of calling the
    // run_preamble / run_reset / run_clear helpers, which carry their
    // own standalone-scene pacing) is intentional.
    note(r, "# tint — terminal theme switcher", TYPE_NORMAL)?;
    blank(r, ms(0))?;
    feature(r)?;
    blank(r, ms(0))?;
    line(r, "tint reset", TYPE_NORMAL, ms(0), ms(0))?;
    // No blank between reset and clear — they sit on subsequent prompt
    // lines so the viewer reads the wrap-up as one tight pair.
    // Type `clear` but pause for a beat with the word visible on the
    // prompt before pressing Enter. Lets the viewer register what's
    // about to happen — the "you've seen everything; clearing now"
    // moment — instead of `clear` flashing past as the screen wipes
    // simultaneously. Identical timing at every subloop boundary
    // (including the loop wrap), so the wrap stays indistinguishable.
    virtual_clear(r, CLEAR_REGISTER)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let (picker_current, down_to_target) = if args.lookup_picker_target {
        (None, lookup_picker_idx(&args.tint_path, PICKER_TARGET)?)
    } else {
        (
            Some(PICKER_START.to_string()),
            PICKER_DEFAULT_DOWN_TO_TARGET,
        )
    };

    // 20 rows: each subloop renders at most preamble (1) + feature
    // (~10–14 rows for cd_hook, the tallest) + reset (1) + cursor.
    // Bump if cd_hook clips.
    let mut r = Recorder::start(RecorderConfig {
        rows: 20,
        picker_current,
        ..RecorderConfig::default()
    })?;

    // Initial prompt sync. Cast time stays at zero, but wall-clock capture
    // waits only until bash actually draws the prompt.
    wait_for_prompt(&mut r, ms(0), "startup prompt")?;

    // Order: cli → picker → cd-hook → custom-theme. cli first because
    // it's the fastest demonstration of the verb; picker is the visually
    // impressive moment but lands harder *after* the viewer already
    // understands the basic form; cd-hook adds automation; custom-theme
    // shows extensibility.
    match args.subloop_only {
        Some(0) => run_subloop(&mut r, run_cli)?,
        Some(1) => run_subloop(&mut r, |r| run_picker(r, down_to_target))?,
        Some(2) => run_subloop(&mut r, run_cd_hook)?,
        Some(3) => run_subloop(&mut r, run_custom_theme)?,
        Some(other) => anyhow::bail!("--subloop-only out of range: {other} (valid: 0..=3)"),
        None => {
            run_subloop(&mut r, run_cli)?;
            run_subloop(&mut r, |r| run_picker(r, down_to_target))?;
            run_subloop(&mut r, run_cd_hook)?;
            run_subloop(&mut r, run_custom_theme)?;
        }
    }

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
