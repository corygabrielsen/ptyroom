//! `ptyrender` CLI: render an existing PTY trace to GIF/MP4.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use ptyrender::render_cli as render;

mod pipeline_cmd;
mod verify_cmd;

#[derive(Parser)]
#[command(
    version,
    about = "ptyrender — render a PTY trace to media",
    override_usage = "ptyrender <trace.ptytrace> <out.gif|out.mp4> [options]\n       ptyrender verify --witness <witness.json> --trace <trace.ptytrace>",
    long_about = "Render an existing `.ptytrace` file to GIF/MP4 media,\n\
                  optionally writing a reproducibility witness and anchoring\n\
                  behavioral specs or provenance attestations.\n\n\
                  Examples:\n\
                    ptyrender <trace.ptytrace> <out.gif|out.mp4> [options]\n\
                    ptyrender verify --witness <witness.json> --trace <trace.ptytrace>"
)]
struct RenderCli {
    #[command(flatten)]
    args: render::Args,
}

#[derive(Parser)]
#[command(
    version,
    about = "ptyrender verify — verify a render witness",
    long_about = "Re-render a trace and verify that it matches a previously\n\
                  issued witness. Optional contract and attestation files\n\
                  are checked when the witness commits to them."
)]
struct VerifyCli {
    #[command(flatten)]
    args: verify_cmd::Args,
}

#[derive(Parser)]
#[command(
    version,
    about = "ptyrender pipeline — per-stage tools for the goldens contract",
    long_about = "Materializes the intermediate artifacts (per-frame JSON,\n\
                  PNGs, encoded media) that tint-scenes/pipeline-test hashes\n\
                  into per-stage goldens. Not the user-facing surface."
)]
struct PipelineCli {
    #[command(subcommand)]
    cmd: pipeline_cmd::Cmd,
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("ptyrender: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> anyhow::Result<bool> {
    let mut argv: Vec<OsString> = std::env::args_os().collect();
    match argv.get(1).and_then(|arg| arg.to_str()) {
        None => {
            RenderCli::command().print_help()?;
            println!();
            Ok(true)
        }
        Some("verify") => {
            argv.remove(1);
            if let Some(program) = argv.get_mut(0) {
                *program = OsString::from("ptyrender verify");
            }
            let cli = VerifyCli::parse_from(argv);
            verify_cmd::run(&cli.args)
        }
        Some("pipeline") => {
            argv.remove(1);
            if let Some(program) = argv.get_mut(0) {
                *program = OsString::from("ptyrender pipeline");
            }
            let cli = PipelineCli::parse_from(argv);
            pipeline_cmd::run(&cli.cmd)?;
            Ok(true)
        }
        Some(_) => {
            let cli = RenderCli::parse_from(argv);
            render::run(&cli.args)?;
            Ok(true)
        }
    }
}
