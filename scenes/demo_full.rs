//! Full 4-feature marketing demo: picker → cli → cd-hook → custom-theme.
//!
//! Composes the per-feature scene helpers from [`tint_recorder::scenes`].
//! Every prerequisite (directories, .tint files, .theme files) is created
//! on screen during the recording. Hermeticity comes from the demo
//! container the recorder spawns.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{
    blank, lookup_picker_idx, ms,
    run_cd_hook, run_cli, run_custom_theme, run_picker, run_preamble,
};

/// Theme the picker lands on. Picked deliberately for the cool/blue
/// register — reads better as the demo's first reveal than a warm/orange
/// theme, which can look default-terminal-ish at a glance.
const PICKER_TARGET: &str = "dark-azure";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/demo_full.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let target_idx = lookup_picker_idx(&args.tint_path, PICKER_TARGET)?;

    // Composition pacing:
    // - 800/600ms initial dwell: bash needs ~600ms to set up echo before
    //   the first keystroke or input bytes leak into the top-left.
    //   Required on every recording's first call (per scenes.rs convention).
    // - run_preamble enumerates the four features as a numbered list, so
    //   the viewer knows what they're investing attention in. Per-act
    //   headers later are bare descriptions (no numbers) — the preamble
    //   already carried the count.
    // - One blank Enter (500ms dwell) between every act for consistent
    //   visual breathing room — anything more reads heavy, anything less
    //   makes acts run together.
    // - 3500ms outro dwell at the end so the final "hot" theme has time
    //   to register before the loop restarts; shorter felt clipped.
    let mut r = Recorder::start(RecorderConfig::default())?;
    r.dwell(ms(800), ms(600))?;
    run_preamble(&mut r)?;
    blank(&mut r, ms(500))?;
    run_picker(&mut r, target_idx)?;
    blank(&mut r, ms(500))?;
    run_cli(&mut r)?;
    blank(&mut r, ms(500))?;
    run_cd_hook(&mut r)?;
    blank(&mut r, ms(500))?;
    run_custom_theme(&mut r)?;
    r.dwell(ms(3500), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
