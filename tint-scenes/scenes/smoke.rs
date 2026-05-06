//! Smoke scene: open picker, scroll a few rows, dismiss with Esc.
use std::path::PathBuf;

use clap::Parser;
use term_recorder::recorder::{Key, Recorder, RecorderConfig};
use tint_scenes::scenes::ms;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/smoke.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        cols: 100,
        rows: 30,
        ..tint_scenes::scenes::tint_recorder_config()
    })?;

    r.dwell(ms(800), ms(600))?;
    r.type_text("tint", ms(80))?;
    r.key(Key::Enter, ms(400))?;
    r.dwell(ms(800), ms(100))?;

    r.keys(Key::Down, ms(120), 5)?;
    r.dwell(ms(500), ms(100))?;

    r.keys(Key::Up, ms(120), 2)?;
    r.dwell(ms(500), ms(100))?;

    r.key(Key::Escape, ms(400))?;
    r.dwell(ms(600), ms(100))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
