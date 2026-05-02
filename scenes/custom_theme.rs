//! Custom-theme feature scene: drop a `.theme` file in the user's themes
//! directory, then apply it by name.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{ms, run_custom_theme};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/custom_theme.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig::default())?;
    r.dwell(ms(800), ms(600))?;
    run_custom_theme(&mut r)?;
    r.dwell(ms(2500), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
