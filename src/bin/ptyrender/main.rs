//! `ptyrender` CLI: render an existing PTY trace to GIF/MP4.

use clap::Parser;

#[path = "../ptytrace/attestation_io.rs"]
mod attestation_io;
#[path = "../ptytrace/render.rs"]
mod render;

#[derive(Parser)]
#[command(
    version,
    about = "ptyrender — render a PTY trace to media",
    long_about = "Render an existing `.ptytrace` file to GIF/MP4 media,\n\
                  optionally writing a reproducibility witness and anchoring\n\
                  behavioral specs or provenance attestations."
)]
struct Cli {
    #[command(flatten)]
    args: render::Args,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    render::run(&cli.args)
}
