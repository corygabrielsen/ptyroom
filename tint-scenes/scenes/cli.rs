//! CLI feature scene: apply built-in themes by name from the command line.

use std::path::PathBuf;

use clap::Parser;
use term_recorder::recorder::{Recorder, RecorderConfig};
use tint_scenes::scenes::{ms, run_cli, run_standalone_feature_subloop, wait_for_prompt};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/cli.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        rows: 20,
        ..tint_scenes::scenes::tint_recorder_config()
    })?;
    wait_for_prompt(&mut r, ms(0), "startup prompt")?;
    run_standalone_feature_subloop(&mut r, run_cli)?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
