//! cd-hook feature scene: install the bash hook, then `cd` into directories
//! whose `.tint` files auto-apply themes.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{ms, run_cd_hook};

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
    r.dwell(ms(800), ms(600))?;
    run_cd_hook(&mut r)?;
    r.dwell(ms(2500), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
