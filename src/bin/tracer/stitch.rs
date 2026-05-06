//! `stitch` subcommand: concatenate N cast files into one, rebasing timestamps.
//!
//! All inputs must share the same width and height. The output's header
//! is taken from the first cast.

use std::path::PathBuf;

use tracer::trace::{Trace, TraceEvent};

#[derive(clap::Args)]
pub struct Args {
    /// Output cast path. Parent directories are created if needed.
    #[arg(long)]
    out: PathBuf,
    /// Input cast paths in stitch order.
    inputs: Vec<PathBuf>,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    if args.inputs.is_empty() {
        anyhow::bail!("stitch: no input casts");
    }

    let casts: Vec<Trace> = args
        .inputs
        .iter()
        .map(Trace::read)
        .collect::<Result<_, _>>()?;

    let (w, h) = (casts[0].header.width, casts[0].header.height);
    for (i, c) in casts.iter().enumerate().skip(1) {
        if c.header.width != w || c.header.height != h {
            anyhow::bail!(
                "stitch: input {} has dimensions {}x{}, expected {}x{} (matching cast 0)",
                args.inputs[i].display(),
                c.header.width,
                c.header.height,
                w,
                h,
            );
        }
    }

    let mut events: Vec<TraceEvent> = Vec::new();
    let mut t_offset = 0.0_f64;
    for cast in &casts {
        for ev in &cast.events {
            events.push(TraceEvent {
                time_s: ev.time_s + t_offset,
                kind: ev.kind,
                data: ev.data.clone(),
            });
        }
        if let Some(last) = cast.events.last() {
            t_offset += last.time_s;
        }
    }

    let stitched = Trace {
        header: casts[0].header.clone(),
        events,
    };
    stitched.write_with_summary(&args.out)
}
