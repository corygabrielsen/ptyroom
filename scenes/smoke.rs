//! Smoke scene: open picker, scroll a few rows, dismiss with Esc.
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use tint_recorder::recorder::{Key, Recorder, RecorderConfig};
use tint_recorder::scenes::ms;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/smoke.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        cols: 100, rows: 30, ..Default::default()
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
    let _ = std::fs::create_dir_all(args.cast.parent().unwrap_or(&PathBuf::from(".")));
    cast.write(&args.cast)?;
    println!("wrote {} ({} bytes, {} events)",
        args.cast.display(),
        std::fs::metadata(&args.cast)?.len(),
        cast.events.len(),
    );
    Ok(())
}

// Quiet a clippy lint for the unused Duration import path
const _: Option<Duration> = None;
