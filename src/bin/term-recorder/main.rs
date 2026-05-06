//! Unified pipeline CLI: `term-recorder <subcommand> ...`.
//!
//! User-facing subcommands sit at the top level. Per-stage pipeline
//! tools (snapshot → paint → encode plus the diff/inspect helpers)
//! live under `term-recorder debug <subcmd>` so the top-level help
//! lists only the surface most users actually reach for.

mod check;
mod compare_snapshots;
mod encode;
mod inspect;
mod paint;
mod rec;
mod record;
mod render;
mod snapshot;
mod stitch;
mod verify;

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
    /// Live: record your real terminal session into a cast (asciinema-shaped UX).
    Rec(rec::Args),
    /// Run a `.scene` file → cast (or chain through render to MP4/GIF).
    Record(record::Args),
    /// Cast → MP4/GIF in one call (with optional reproducibility receipt).
    Render(render::Args),
    /// Concatenate N cast files into one, rebasing event timestamps.
    Stitch(stitch::Args),
    /// Verify a previously-issued reproducibility receipt by re-rendering.
    Verify(verify::Args),
    /// Replay a cast and check it against a behavioral spec.
    Check(check::Args),
    /// Per-stage pipeline tools (snapshot, paint, encode, inspect, compare-snapshots).
    #[command(subcommand)]
    Debug(DebugCmd),
}

/// Pipeline internals. These expose the individual stages of `render`
/// (snapshot → paint → encode) plus the diff / inspect helpers. Useful
/// when you want intermediate artifacts on disk or you're debugging a
/// determinism gap; for everyday use, `render` chains them in memory.
#[derive(Subcommand)]
enum DebugCmd {
    /// Cast → per-frame snapshot JSON + timing.json.
    Snapshot(snapshot::Args),
    /// Snapshots directory → painted PNGs.
    Paint(paint::Args),
    /// PNG sequence + timing.json → GIF/MP4.
    Encode(encode::Args),
    /// Frame-by-frame A/B comparison of replayed snapshot directories.
    CompareSnapshots(compare_snapshots::Args),
    /// ASCII-render any snapshot to the terminal.
    Inspect(inspect::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result: anyhow::Result<ExitCode> = match cli.cmd {
        Cmd::Rec(args) => rec::run(args).map(|()| ExitCode::SUCCESS),
        Cmd::Record(args) => record::run(&args).map(|()| ExitCode::SUCCESS),
        Cmd::Render(args) => render::run(&args).map(|()| ExitCode::SUCCESS),
        Cmd::Stitch(args) => stitch::run(&args).map(|()| ExitCode::SUCCESS),
        Cmd::Verify(args) => verify::run(&args).map(bool_to_exit),
        Cmd::Check(args) => check::run(&args).map(bool_to_exit),
        Cmd::Debug(sub) => match sub {
            DebugCmd::Snapshot(args) => snapshot::run(&args).map(|()| ExitCode::SUCCESS),
            DebugCmd::Paint(args) => paint::run(&args).map(|()| ExitCode::SUCCESS),
            DebugCmd::Encode(args) => encode::run(args).map(|()| ExitCode::SUCCESS),
            DebugCmd::CompareSnapshots(args) => compare_snapshots::run(&args).map(bool_to_exit),
            DebugCmd::Inspect(args) => inspect::run(&args).map(|()| ExitCode::SUCCESS),
        },
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("term-recorder: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn bool_to_exit(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
