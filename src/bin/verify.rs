//! CLI: run a scene's verification contract against the recorded snapshots.
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tint_recorder::contracts::registry;
use tint_recorder::verify::load_snapshots_dir;

#[derive(Parser)]
struct Args {
    /// Scene name (e.g. `demo_full`, `smoke`).
    scene: String,
    /// Snapshots directory (default: `assets/snapshots`).
    #[arg(long, default_value = "assets/snapshots")]
    snapshots_dir: PathBuf,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => code,
        Err(e) => { eprintln!("verify: {e:#}"); ExitCode::from(2) }
    }
}

fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let contract = registry(&args.scene)
        .ok_or_else(|| anyhow::anyhow!("no contract defined for scene {:?}", args.scene))?;
    let snaps = load_snapshots_dir(&args.snapshots_dir)?;
    let report = contract.run(&snaps);
    report.print();
    Ok(ExitCode::from(report.exit_code() as u8))
}
