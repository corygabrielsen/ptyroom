//! Picker feature scene: open the interactive picker, scroll to a target
//! theme, accept.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{ms, run_picker};

const PICKER_START: &str = "dark-sky-blue";
const PICKER_DEFAULT_DOWN_TO_TARGET: usize = 1;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/picker.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut r = Recorder::start(RecorderConfig {
        picker_current: Some(PICKER_START.to_string()),
        ..RecorderConfig::default()
    })?;
    r.dwell(ms(800), ms(600))?; // bash echo setup, per scenes.rs convention
    run_picker(&mut r, PICKER_DEFAULT_DOWN_TO_TARGET)?;
    r.dwell(ms(2500), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
