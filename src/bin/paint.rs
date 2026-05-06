//! CLI: snapshots dir → frames dir of PNGs.
//!
//! Frames are painted in parallel. Each frame is independent of every
//! other (load snapshot, paint to RGB image, save PNG), so rayon's
//! `par_iter` scales linearly with available cores. The painter struct
//! is `Sync` (font + scale + immutable metrics), shareable across
//! worker threads without locking.
use std::path::PathBuf;

use clap::Parser;
use rayon::prelude::*;
use term_recorder::paint::{FONT_BYTES, PaintConfig, Painter};
use term_recorder::snapshot::Snapshot;
use term_recorder::verify::list_numbered_snapshots;

#[derive(Parser)]
struct Args {
    snap_dir: PathBuf,
    out_dir: PathBuf,
    #[arg(long, default_value_t = 14.0)]
    font_size: f32,
    #[arg(long, default_value_t = 12)]
    padding: u32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out_dir)?;

    let painter = Painter::new(
        FONT_BYTES,
        PaintConfig {
            font_size_px: args.font_size,
            padding_px: args.padding,
            cell_w_px: None,
            cell_h_px: None,
        },
    )?;

    let entries = list_numbered_snapshots(&args.snap_dir)?;
    let m = painter.metrics();
    println!(
        "painting {} frames at cell {}x{}",
        entries.len(),
        m.width,
        m.height
    );

    entries
        .par_iter()
        .try_for_each(|path| -> anyhow::Result<()> {
            let snap = Snapshot::load(path)?;
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            let out = args.out_dir.join(format!("{stem}.png"));
            painter.save_png(&snap, &out)?;
            Ok(())
        })?;

    println!("wrote PNGs to {}", args.out_dir.display());
    Ok(())
}
