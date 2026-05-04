//! Picker feature scene: open the interactive picker, scroll to a target
//! theme, accept.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{lookup_picker_idx, ms, run_picker};

const PICKER_TARGET: &str = "dark-azure";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/picker.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let down_to_target = lookup_picker_idx(&args.tint_path, PICKER_TARGET)?;

    let mut r = Recorder::start(RecorderConfig {
        rows: 20,
        ..RecorderConfig::default()
    })?;
    r.dwell(ms(800), ms(600))?; // bash echo setup, per scenes.rs convention
    run_picker(&mut r, down_to_target)?;
    r.dwell(ms(2500), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
