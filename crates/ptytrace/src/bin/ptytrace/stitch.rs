//! `stitch` subcommand: concatenate N trace files into one, rebasing timestamps.
//!
//! All inputs must share the same width and height. The output's header
//! is taken from the first trace.

use std::path::PathBuf;

use ptytrace::trace::{Trace, TraceEvent};

#[derive(clap::Args)]
pub struct Args {
    /// Output trace path. Parent directories are created if needed.
    #[arg(long)]
    out: PathBuf,
    /// Input trace paths in stitch order.
    inputs: Vec<PathBuf>,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    if args.inputs.is_empty() {
        anyhow::bail!("stitch: no input traces");
    }

    let traces: Vec<Trace> = args
        .inputs
        .iter()
        .map(Trace::read)
        .collect::<Result<_, _>>()?;

    let (w, h) = (traces[0].header.width, traces[0].header.height);
    for (i, c) in traces.iter().enumerate().skip(1) {
        if c.header.width != w || c.header.height != h {
            anyhow::bail!(
                "stitch: input {} has dimensions {}x{}, expected {}x{} (matching trace 0)",
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
    for trace in &traces {
        for ev in &trace.events {
            events.push(TraceEvent {
                time_s: ev.time_s + t_offset,
                kind: ev.kind,
                data: ev.data.clone(),
            });
        }
        if let Some(last) = trace.events.last() {
            t_offset += last.time_s;
        }
    }

    let stitched = Trace {
        header: traces[0].header.clone(),
        events,
    };
    stitched.write(&args.out)?;
    println!(
        "wrote {} ({} bytes, {} events)",
        args.out.display(),
        std::fs::metadata(&args.out)?.len(),
        stitched.events.len()
    );
    Ok(())
}
