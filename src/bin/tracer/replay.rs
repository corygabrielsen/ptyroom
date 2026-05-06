//! `snapshot` subcommand: cast → per-frame snapshot JSON + timing.json.

use std::fs;
use std::path::PathBuf;

use tracer::frame_replay::replay;
use tracer::trace::Trace;
use tracer::tracer::StubColors;

#[derive(clap::Args)]
pub struct Args {
    /// Trace file (asciinema v2 JSONL).
    cast: PathBuf,
    /// Output directory; created if absent. Receives one JSON file per
    /// cast `"o"` event plus `timing.json`.
    out_dir: PathBuf,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let cast = Trace::read(&args.cast)?;
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
