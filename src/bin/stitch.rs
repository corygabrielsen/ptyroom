//! CLI: concatenate N cast files into one, rebasing event timestamps.
//!
//! Each input cast contributes a contiguous block of events to the
//! output. The first cast's events are copied verbatim; subsequent
//! casts have their timestamps offset by the cumulative end-time of
//! all preceding casts, so the merged timeline is monotonically
//! increasing and gap-free.
//!
//! Used by the parallel-record demo flow: split a multi-subloop demo
//! into N independent scene binaries, record them concurrently, then
//! stitch the per-subloop casts into a single combined cast that the
//! normal paint/encode/verify pipeline consumes.
//!
//! All input casts must share the same width and height — they're
//! recorded in identically-sized terminals so the renderer's
//! per-frame layout stays consistent. The output uses the first
//! cast's header.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::cast::{Cast, CastEvent};

#[derive(Parser)]
struct Args {
    /// Output cast path. Parent directories are created if needed.
    #[arg(long)]
    out: PathBuf,
    /// Input cast paths in stitch order. The output's header is taken
    /// from the first cast.
    inputs: Vec<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.inputs.is_empty() {
        anyhow::bail!("stitch: no input casts");
    }

    let casts: Vec<Cast> = args.inputs.iter()
        .map(Cast::read)
        .collect::<Result<_, _>>()?;

    // All inputs must share dimensions. Mismatched dimensions would
    // produce a cast that can't be replayed coherently (frames at
    // different sizes within one stream).
    let (w, h) = (casts[0].header.width, casts[0].header.height);
    for (i, c) in casts.iter().enumerate().skip(1) {
        if c.header.width != w || c.header.height != h {
            anyhow::bail!(
                "stitch: input {} has dimensions {}x{}, expected {}x{} (matching cast 0)",
                args.inputs[i].display(), c.header.width, c.header.height, w, h,
            );
        }
    }

    let mut events: Vec<CastEvent> = Vec::new();
    let mut t_offset = 0.0_f64;
    for cast in &casts {
        for ev in &cast.events {
            events.push(CastEvent {
                time_s: ev.time_s + t_offset,
                kind: ev.kind,
                data: ev.data.clone(),
            });
        }
        // Advance the offset by the last event's timestamp in this cast,
        // so the next cast's t=0 picks up immediately after this cast's
        // final event. If a cast has no events, no offset advance — but
        // a recorder that wrote zero events would already be a bug.
        if let Some(last) = cast.events.last() {
            t_offset += last.time_s;
        }
    }

    let stitched = Cast { header: casts[0].header.clone(), events };
    stitched.write_with_summary(&args.out)?;
    Ok(())
}
