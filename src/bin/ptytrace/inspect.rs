//! `inspect` subcommand: ASCII-render any snapshot to the terminal.

use std::path::PathBuf;

use ptytrace::frame::Frame;
use ptytrace::inspect::{InspectMode, RowRange, render};

#[derive(clap::Args)]
pub struct Args {
    snapshot: PathBuf,
    /// Emit ANSI 24-bit color (bg + fg) per cell.
    #[arg(long)]
    color: bool,
    /// Row range as `start:end` / `:end` / `start:` / `N` (default: all rows).
    #[arg(long, default_value = ":")]
    rows: String,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let snap = Frame::load(&args.snapshot)?;
    let range = RowRange::parse(&args.rows, snap.rows()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mode = if args.color {
        InspectMode::Color
    } else {
        InspectMode::Plain
    };
    eprintln!(
        "{}: bg={} fg={} {}x{}",
        args.snapshot.display(),
        snap.bg,
        snap.fg,
        snap.rows(),
        snap.cols()
    );
    print!("{}", render(&snap, range, mode));
    Ok(())
}
