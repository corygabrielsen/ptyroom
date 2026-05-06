//! Full 4-feature marketing demo, restructured into per-feature subloops.
//!
//! Each feature gets its own self-contained mini-demo:
//!   preamble → feature → reset → clear
//!
//! Because every clear-to-next-preamble seam has identical timing and
//! identical post-clear terminal state (blank + PS1 + cursor at row 1),
//! the loop wrap is visually indistinguishable from any inter-subloop
//! transition.
//!
//! Side benefit: each subloop is short, so the recording's row count
//! drops from the 36 the stacked-acts version needed down to ~20.

use std::path::PathBuf;

use clap::Parser;
use term_recorder::recorder::{Recorder, RecorderConfig};
use tint_scenes::scenes::{
    lookup_picker_idx, ms, run_cd_hook, run_cli, run_custom_theme, run_feature_subloop, run_picker,
    wait_for_prompt,
};

/// Theme the picker lands on. Cool/blue register reads better as the
/// reveal than a warm/orange theme, which can look default-terminal-ish.
const PICKER_TARGET: &str = "dark-azure";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/demo_full.cast")]
    cast: PathBuf,
    /// Record only one subloop (0=cli, 1=cd-hook, 2=picker, 3=custom-theme).
    /// Used by the parallel-record flow: spawn 4 copies of this binary
    /// concurrently with --subloop-only 0..3 each writing its own cast,
    /// then stitch the resulting casts. Each subloop is self-contained
    /// (ends in `clear`) so per-subloop casts splice cleanly.
    #[arg(long)]
    subloop_only: Option<usize>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let down_to_target = lookup_picker_idx(&args.tint_path, PICKER_TARGET)?;

    // 20 rows: each subloop renders at most preamble (1) + feature
    // (~10–14 rows for cd_hook, the tallest) + reset (1) + cursor.
    // Bump if cd_hook clips.
    let mut r = Recorder::start(RecorderConfig {
        rows: 20,
        ..tint_scenes::scenes::tint_recorder_config()
    })?;

    // Initial prompt sync. Cast time stays at zero, but wall-clock capture
    // waits only until bash actually draws the prompt.
    wait_for_prompt(&mut r, ms(0), "startup prompt")?;

    // Order: cli → cd-hook → picker → custom-theme. cli first because
    // it's the fastest demonstration of the verb; cd-hook second because
    // it's the most distinctive feature and lands strongest while the
    // viewer is still fresh; picker is the visually impressive moment
    // but reads as "and there's a picker too"; custom-theme shows
    // extensibility.
    match args.subloop_only {
        Some(0) => run_feature_subloop(&mut r, run_cli)?,
        Some(1) => run_feature_subloop(&mut r, run_cd_hook)?,
        Some(2) => run_feature_subloop(&mut r, |r| run_picker(r, down_to_target))?,
        Some(3) => run_feature_subloop(&mut r, run_custom_theme)?,
        Some(other) => anyhow::bail!("--subloop-only out of range: {other} (valid: 0..=3)"),
        None => {
            run_feature_subloop(&mut r, run_cli)?;
            run_feature_subloop(&mut r, run_cd_hook)?;
            run_feature_subloop(&mut r, |r| run_picker(r, down_to_target))?;
            run_feature_subloop(&mut r, run_custom_theme)?;
        }
    }

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
