//! Unified tracer CLI: `tracer <subcommand> ...`.
//!
//! User-facing subcommands sit at the top level. Per-stage pipeline
//! tools (replay → paint → encode plus the diff/inspect helpers)
//! live under `tracer debug <subcmd>` so the top-level help lists
//! only the surface most users actually reach for.

mod capture;
mod check;
mod compare_frames;
mod demo;
mod encode;
mod inspect;
mod paint;
mod render;
mod replay;
mod run;
mod stitch;
mod verify;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    version,
    about = "tracer — deterministic terminal-session recorder",
    long_about = "Run with no subcommand for the full demo flow: capture a live\n\
                  terminal session, render it to a GIF, produce a witness, open\n\
                  the GIF. Subcommands expose individual stages."
)]
struct Cli {
    /// Subcommand to run. Omit for the bare `tracer` demo flow:
    /// capture → render → witness → open the GIF.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Live: capture your real terminal session into a trace (asciinema-shaped UX).
    Capture(capture::Args),
    /// Run a `.script` file → trace (or chain through render to MP4/GIF).
    Run(run::Args),
    /// Trace → MP4/GIF in one call (with optional reproducibility witness).
    Render(render::Args),
    /// Concatenate N traces into one, rebasing event timestamps (the trace-monoid ⊕).
    Stitch(stitch::Args),
    /// Verify a previously-issued reproducibility witness by re-rendering.
    Verify(verify::Args),
    /// Replay a trace and check it against a behavioral contract.
    Check(check::Args),
    /// Per-stage pipeline tools (replay, paint, encode, inspect, compare-frames).
    #[command(subcommand)]
    Debug(DebugCmd),
}

/// Pipeline internals. These expose the individual stages of `render`
/// (replay → paint → encode) plus the diff / inspect helpers. Useful
/// when you want intermediate artifacts on disk or you're debugging a
/// determinism gap; for everyday use, `render` chains them in memory.
#[derive(Subcommand)]
enum DebugCmd {
    /// Trace → per-frame state JSON + timing.json.
    Replay(replay::Args),
    /// Frames directory → painted PNGs.
    Paint(paint::Args),
    /// PNG sequence + timing.json → GIF/MP4.
    Encode(encode::Args),
    /// Frame-by-frame A/B comparison of replayed frame directories.
    CompareFrames(compare_frames::Args),
    /// ASCII-render any frame to the terminal.
    Inspect(inspect::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result: anyhow::Result<ExitCode> = match cli.cmd {
        None => demo::run().map(|()| ExitCode::SUCCESS),
        Some(Cmd::Capture(args)) => capture::run(args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Run(args)) => run::run(&args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Render(args)) => render::run(&args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Stitch(args)) => stitch::run(&args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Verify(args)) => verify::run(&args).map(bool_to_exit),
        Some(Cmd::Check(args)) => check::run(&args).map(bool_to_exit),
        Some(Cmd::Debug(sub)) => match sub {
            DebugCmd::Replay(args) => replay::run(&args).map(|()| ExitCode::SUCCESS),
            DebugCmd::Paint(args) => paint::run(&args).map(|()| ExitCode::SUCCESS),
            DebugCmd::Encode(args) => encode::run(args).map(|()| ExitCode::SUCCESS),
            DebugCmd::CompareFrames(args) => compare_frames::run(&args).map(bool_to_exit),
            DebugCmd::Inspect(args) => inspect::run(&args).map(|()| ExitCode::SUCCESS),
        },
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("tracer: {err:#}");
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
