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
    blank, lookup_picker_idx, ms, run_cd_hook, run_cli, run_custom_theme, run_picker,
};

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

    // Single blank line between every act for consistent visual spacing.
    let mut r = Recorder::start(RecorderConfig::default())?;
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
