//! Unified pipeline CLI: `term-recorder <subcommand> ...`.

mod compare_snapshots;
mod encode;
mod inspect;
mod paint;
mod snapshot;
mod stitch;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "term-recorder pipeline tools")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// PNG sequence + timing.json → GIF/MP4.
    Encode(encode::Args),
    /// Snapshots directory → painted PNGs.
    Paint(paint::Args),
    /// Cast → per-frame snapshot JSON + timing.json.
    Snapshot(snapshot::Args),
    /// Concatenate N cast files into one, rebasing event timestamps.
    Stitch(stitch::Args),
    /// Frame-by-frame A/B comparison of replayed snapshot directories.
    CompareSnapshots(compare_snapshots::Args),
    /// ASCII-render any snapshot to the terminal.
    Inspect(inspect::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result: anyhow::Result<ExitCode> = match cli.cmd {
        Cmd::Encode(args) => encode::run(args).map(|()| ExitCode::SUCCESS),
        Cmd::Paint(args) => paint::run(&args).map(|()| ExitCode::SUCCESS),
        Cmd::Snapshot(args) => snapshot::run(&args).map(|()| ExitCode::SUCCESS),
        Cmd::Stitch(args) => stitch::run(&args).map(|()| ExitCode::SUCCESS),
        Cmd::CompareSnapshots(args) => compare_snapshots::run(&args).map(|ok| {
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }),
        Cmd::Inspect(args) => inspect::run(&args).map(|()| ExitCode::SUCCESS),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("term-recorder: {err:#}");
            ExitCode::from(2)
        }
    }
}
