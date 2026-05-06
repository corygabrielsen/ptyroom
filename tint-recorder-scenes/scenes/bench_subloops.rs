//! Subloop-pattern benchmark — sequential baseline for the
//! parallelizable demo pattern.
//!
//! Runs N uniform subloops sequentially, each structured as
//! preamble → synthetic content → reset → clear. This is the *before*
//! measurement for a future parallelize-and-stitch refactor of the
//! same pattern: today we record all N subloops in one Recorder; later
//! we'll record each subloop in its own Recorder concurrently and
//! stitch the casts.
//!
//! Why uniform synthetic content instead of `demo_full`'s bespoke
//! per-feature acts: this scene controls for content complexity. The
//! only variable is subloop count, so wall-time should scale linearly
//! with N today and (ideally) be constant in N after parallelization.
//! Compare:
//!
//!     time make bench-subloops          # default N=4, matches demo_full
//!     ./target/release/bench_subloops --n 1 --cast /tmp/n1.cast
//!     ./target/release/bench_subloops --n 8 --cast /tmp/n8.cast
//!
//! Open contract: no verify checks. The scene's output is a pacing
//! baseline, not a correctness check.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder_scenes::scenes::{blank, line, ms};

/// One synthetic subloop. Designed to take ~5 seconds of cast time:
/// preamble + 3 short typed commands with brief settles + reset +
/// clear. Pure typing rhythm matches `demo_full`'s per-subloop shape.
/// Uniform across all N invocations so timing is purely a function
/// of how many we run, not what's in them.
fn run_synthetic_subloop(r: &mut Recorder, idx: usize) -> anyhow::Result<()> {
    let header = format!("# bench_subloops [{idx}]");
    line(r, &header, ms(28), ms(0), ms(0))?;
    blank(r, ms(0))?;

    // Three quick `tint <theme>` lines per subloop. Each has an
    // 800ms settle so the bg flip is visible in the cast — same
    // shape as demo_full's act lines, without bespoke per-feature
    // logic (picker, cd-hook, heredoc).
    line(r, "tint dracula", ms(35), ms(0), ms(800))?;
    line(r, "tint solarized-light", ms(35), ms(0), ms(800))?;
    line(r, "tint monokai", ms(35), ms(0), ms(800))?;

    blank(r, ms(0))?;
    line(r, "tint reset", ms(35), ms(0), ms(0))?;
    blank(r, ms(0))?;
    line(r, "clear", ms(50), ms(0), ms(0))?;
    Ok(())
}

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/bench_subloops.cast")]
    cast: PathBuf,
    /// Number of subloops to run sequentially. Default 4 matches
    /// `demo_full`'s shape. Vary to measure how wall-time scales with
    /// subloop count.
    #[arg(long, default_value_t = 4)]
    n: usize,
    /// Run only one subloop with the given index. Used by the parallel
    /// stitch flow: spawn N copies of this binary concurrently, each
    /// recording one subloop into its own cast, then stitch the
    /// resulting casts. Each subloop is self-contained (ends with
    /// `clear`) so per-subloop casts splice cleanly.
    #[arg(long)]
    subloop_only: Option<usize>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        cols: 80,
        rows: 20,
        ..Default::default()
    })?;

    // Initial bash-echo settle, invisible to the rendered output.
    r.dwell(ms(0), ms(600))?;

    if let Some(idx) = args.subloop_only {
        run_synthetic_subloop(&mut r, idx)?;
    } else {
        for i in 0..args.n {
            run_synthetic_subloop(&mut r, i)?;
        }
    }

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
