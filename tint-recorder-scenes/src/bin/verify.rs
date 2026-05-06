//! CLI: run a scene's verification contract against the recorded snapshots.
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tint_recorder_scenes::contracts::{SCENES, registry};
use tint_recorder::verify::load_snapshots_dir;

#[derive(Parser)]
struct Args {
    /// Scene name (e.g. `demo_full`, `smoke`). Required unless `--list-scenes`.
    scene: Option<String>,
    /// Snapshots directory (default: `assets/snapshots`).
    #[arg(long, default_value = "assets/snapshots")]
    snapshots_dir: PathBuf,
    /// Print every registered scene name (one per line) and exit. Suitable
    /// for piping into a shell loop.
    #[arg(long)]
    list_scenes: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("verify: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> anyhow::Result<ExitCode> {
    if args.list_scenes {
        for name in SCENES {
            println!("{name}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let scene = args
        .scene
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("scene argument required (or pass --list-scenes)"))?;
    let contract = registry(scene)
        .ok_or_else(|| anyhow::anyhow!("no contract defined for scene {scene:?}"))?;
    let snaps = load_snapshots_dir(&args.snapshots_dir)?;
    let report = contract.run(&snaps);
    report.print();
    let code = u8::try_from(report.exit_code()).unwrap_or(1);
    Ok(ExitCode::from(code))
}
