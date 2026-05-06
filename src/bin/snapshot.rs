//! CLI: cast → per-frame snapshot JSON + timing.json.
//!
//! Drop-in replacement for `renderer/snapshot.ts`. Same input/output
//! shape: pass a cast file and an output directory; receive
//! `<outdir>/0001.json … NNNN.json` (one per `"o"` event, 1-indexed
//! by event index in the cast) plus `<outdir>/timing.json`. Backed
//! by the `term_recorder::snapshot_replay` module (vt100 + OscTracker).

use std::fs;
use std::path::PathBuf;

use clap::Parser;
use term_recorder::cast::Cast;
use term_recorder::recorder::StubColors;
use term_recorder::snapshot_replay::replay;

#[derive(Parser)]
struct Args {
    /// Cast file written by a `term-recorder` scene (asciinema v2 JSONL).
    cast: PathBuf,
    /// Output directory; created if absent. Receives one JSON file per
    /// cast `"o"` event plus `timing.json`.
    out_dir: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cast = Cast::read(&args.cast)?;
    let (snapshots, timing) = replay(&cast, StubColors::default())?;

    fs::create_dir_all(&args.out_dir)?;
    for (snap, entry) in snapshots.iter().zip(&timing) {
        let path = args.out_dir.join(format!("{}.json", entry.frame));
        fs::write(&path, serde_json::to_string(snap)?)?;
    }
    fs::write(
        args.out_dir.join("timing.json"),
        serde_json::to_string_pretty(&timing)?,
    )?;
    println!(
        "wrote {} snapshots to {}",
        timing.len(),
        args.out_dir.display()
    );
    Ok(())
}
