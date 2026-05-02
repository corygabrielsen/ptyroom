//! Picker feature scene: open the interactive picker, scroll to a target
//! theme, accept.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{lookup_picker_idx, ms, run_picker};

const PICKER_TARGET: &str = "dark-orange";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/picker.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let target_idx = lookup_picker_idx(&args.tint_path, PICKER_TARGET)?;

    let mut r = Recorder::start(RecorderConfig::default())?;
    run_picker(&mut r, target_idx)?;
    r.dwell(ms(2500), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
