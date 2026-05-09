//! Unified ptytrace CLI: `ptytrace <command...>` or `ptytrace <subcommand> ...`.
//!
//! This binary owns the raw trace-producing operations. Unknown
//! subcommands are treated as argv to record under a PTY, so
//! `ptytrace htop` is the raw trace primitive. Media rendering and
//! witness verification live in the sibling `ptyrender` binary.

mod attest;
mod capture;
mod check;
mod run;
mod stitch;

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(
    version,
    about = "ptytrace — deterministic terminal-session recorder",
    long_about = "Run `ptytrace <command...>` to capture a command under a PTY\n\
                  and write a trace. Named subcommands expose trace capture,\n\
                  scripted recording, stitching, attestations, and contract checks."
)]
struct Cli {
    /// Subcommand to run. Unknown subcommands are treated as command argv:
    /// `ptytrace ssh host`, `ptytrace htop`, etc.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Live: capture your real terminal session into a trace (asciinema-shaped UX).
    Capture(capture::Args),
    /// Run a `.script` file and write a trace.
    Run(run::Args),
    /// Produce a detached provenance attestation for a trace.
    Attest(attest::Args),
    /// Concatenate N traces into one, rebasing event timestamps (the trace-monoid ⊕).
    Stitch(stitch::Args),
    /// Replay a trace and check it against a behavioral contract.
    Check(check::Args),
    /// Raw command passthrough: `ptytrace ssh host`, `ptytrace htop`, etc.
    #[command(external_subcommand)]
    Command(Vec<String>),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result: anyhow::Result<ExitCode> = match cli.cmd {
        None => {
            let mut cmd = Cli::command();
            match cmd.print_help() {
                Ok(()) => {
                    println!();
                    Ok(ExitCode::SUCCESS)
                }
                Err(err) => Err(err.into()),
            }
        }
        Some(Cmd::Capture(args)) => capture::run(args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Run(args)) => run::run(&args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Attest(args)) => attest::run(&args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Stitch(args)) => stitch::run(&args).map(|()| ExitCode::SUCCESS),
        Some(Cmd::Check(args)) => check::run(&args).map(bool_to_exit),
        Some(Cmd::Command(argv)) => capture::run_command(argv).map(|()| ExitCode::SUCCESS),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("ptytrace: {err:#}");
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
