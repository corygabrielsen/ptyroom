//! cd-hook feature scene: install the bash hook, then `cd` into directories
//! whose `.tint` files auto-apply themes.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{
    ms, run_cd_hook_feature, run_standalone_feature_subloop, wait_for_prompt,
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/cd_hook.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        rows: 20,
        ..RecorderConfig::default()
    })?;
    wait_for_prompt(&mut r, ms(0), "startup prompt")?;
    run_standalone_feature_subloop(&mut r, run_cd_hook_feature)?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
