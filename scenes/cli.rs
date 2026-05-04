//! CLI feature scene: apply built-in themes by name from the command line.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{ms, run_cli};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/cli.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        rows: 20,
        ..RecorderConfig::default()
    })?;
    r.dwell(ms(800), ms(600))?;
    run_cli(&mut r)?;
    r.dwell(ms(2500), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
