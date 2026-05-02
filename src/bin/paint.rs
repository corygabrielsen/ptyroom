//! CLI: snapshots dir → frames dir of PNGs.
use std::path::PathBuf;

use clap::Parser;
use tint_recorder::paint::{FONT_BYTES, PaintConfig, Painter};
use tint_recorder::snapshot::Snapshot;

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
        PaintConfig { font_size_px: args.font_size, padding_px: args.padding,
                      cell_w_px: None, cell_h_px: None },
    )?;

    let mut entries: Vec<_> = std::fs::read_dir(&args.snap_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .filter(|p| p.file_stem().and_then(|s| s.to_str())
                     .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())))
        .collect();
    entries.sort();

    let m = painter.metrics();
    println!("painting {} frames at cell {}x{}", entries.len(), m.width, m.height);
    for path in entries {
        let snap = Snapshot::load(&path)?;
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let out = args.out_dir.join(format!("{stem}.png"));
        painter.save_png(&snap, &out)?;
    }
    println!("wrote PNGs to {}", args.out_dir.display());
    Ok(())
}
