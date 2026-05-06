//! Custom-theme feature scene: drop a `.theme` file in the user's themes
//! directory, then apply it by name.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder_scenes::scenes::{
    ms, run_custom_theme, run_standalone_feature_subloop, wait_for_prompt,
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/custom_theme.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        rows: 20,
        ..RecorderConfig::default()
    })?;
    wait_for_prompt(&mut r, ms(0), "startup prompt")?;
    run_standalone_feature_subloop(&mut r, run_custom_theme)?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
