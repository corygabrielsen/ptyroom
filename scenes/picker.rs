//! Picker feature scene: open the interactive picker, scroll to a target
//! theme, accept.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{
    lookup_picker_idx, ms, run_picker, run_standalone_feature_subloop, wait_for_prompt,
};

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
    wait_for_prompt(&mut r, ms(0), "startup prompt")?;
    run_standalone_feature_subloop(&mut r, |r| run_picker(r, down_to_target))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
