//! Tiny benchmark scene for pipeline performance measurement.
//!
//! Minimal content: bash settle, two short typed lines, brief dwells.
//! Runs in ~3s of cast time and emits ~30 events. Used to measure the
//! pipeline's *fixed* overhead — tsx startup, paint init, ffmpeg
//! cold-start, docker run setup. The recording itself is bounded by
//! real-time dwells; the rest of the wall clock is overhead.
//!
//! Pair with `bench_churn` to separate fixed costs from per-frame work.
//!
//! Open contract: no verify checks. Pipeline runs end-to-end and exits
//! with zero failures regardless of output content.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{line, ms};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/bench_tiny.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        cols: 80,
        rows: 12,
        ..Default::default()
    })?;

    // Initial bash-echo settle, invisible to the rendered output.
    r.dwell(ms(0), ms(600))?;

    line(
        &mut r,
        "# bench_tiny: minimal pipeline test",
        ms(28),
        ms(300),
        ms(600),
    )?;
    line(&mut r, "echo hello", ms(35), ms(300), ms(800))?;

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
