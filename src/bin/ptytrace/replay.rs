//! `replay` subcommand: trace → per-frame snapshot JSON + timing.json.

use std::fs;
use std::path::PathBuf;

use ptytrace::frame_replay::replay;
use ptytrace::pty::StubColors;
use ptytrace::trace::Trace;

#[derive(clap::Args)]
pub struct Args {
    /// Trace file (asciinema v2 JSONL).
    trace: PathBuf,
    /// Output directory; created if absent. Receives one JSON file per
    /// trace `"o"` event plus `timing.json`.
    out_dir: PathBuf,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let trace = Trace::read(&args.trace)?;
    let (snapshots, timing) = replay(&trace, StubColors::default())?;

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
