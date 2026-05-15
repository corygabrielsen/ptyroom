//! `pipeline` subcommand namespace: per-stage tools backing the goldens
//! contract.
//!
//! These are not user-facing — those subcommands are `render` and
//! `verify`. The `pipeline` namespace materializes the intermediate
//! artifacts (per-frame JSON, PNGs, media) that
//! `tint-scenes/src/pipeline_test.rs` hashes into `goldens/<scene>.json`
//! to assert per-stage byte stability.
//!
//! Historically these lived as `ptytrace debug {replay,paint,encode}`
//! (commit `fb13232` in tint-scenes). The library functions migrated
//! into `ptyrender` during the workspace restructure but the CLI
//! surface didn't follow, breaking `make verify-goldens`. This module
//! restores the surface on its new home.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rayon::prelude::*;

use ptyrender::encode::{EncodeRequest, Mp4Encoder, TimingEntry, encode};
use ptyrender::frame::Frame;
use ptyrender::frame_replay::replay;
use ptyrender::paint::{FONT_BYTES, PaintConfig, Painter};
use ptyrender::verify::list_numbered_snapshots;
use ptytrace::pty::StubColors;
use ptytrace::trace::Trace;

#[derive(Subcommand)]
pub enum Cmd {
    /// Replay a trace through vt100 + `OscTracker`, write per-frame JSON
    /// snapshots and a `timing.json` schedule.
    Replay(ReplayArgs),
    /// Render each JSON snapshot to a PNG. Parallel via rayon.
    Paint(PaintArgs),
    /// Encode a frame directory + timing schedule to MP4 or GIF
    /// (format chosen from the output path's extension).
    Encode(EncodeArgs),
}

#[derive(Args)]
pub struct ReplayArgs {
    /// Input trace file (asciinema v2 JSONL).
    trace: PathBuf,
    /// Output directory. Created if missing. Receives `<NNNN>.json`
    /// per visible event plus `timing.json`.
    snaps_dir: PathBuf,
}

#[derive(Args)]
pub struct PaintArgs {
    /// Directory of `<NNNN>.json` snapshots produced by `pipeline replay`.
    snaps_dir: PathBuf,
    /// Output directory. Created if missing.
    frames_dir: PathBuf,
    /// Font size in pixels.
    #[arg(long, default_value_t = 14.0)]
    font_size: f32,
    /// Padding around the grid in pixels.
    #[arg(long, default_value_t = 12)]
    padding: u32,
}

#[derive(Args)]
pub struct EncodeArgs {
    /// Directory of `<NNNN>.png` frames.
    frames_dir: PathBuf,
    /// Path to `timing.json` describing playback order + dwell per frame.
    timing: PathBuf,
    /// Output media path. `.mp4` or `.gif` chooses the encoder.
    out: PathBuf,
    /// Output frame rate.
    #[arg(long, default_value_t = 25)]
    fps: u32,
    /// Optional output width (lanczos scale). Height auto-computed.
    #[arg(long)]
    width: Option<u32>,
}

/// # Errors
/// Subcommand-specific. See the per-stage runners.
pub fn run(cmd: &Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Replay(a) => run_replay(a),
        Cmd::Paint(a) => run_paint(a),
        Cmd::Encode(a) => run_encode(a),
    }
}

fn run_replay(args: &ReplayArgs) -> anyhow::Result<()> {
    let trace = Trace::read(&args.trace)?;
    let (frames, timing) = replay(&trace, StubColors::default())?;
    std::fs::create_dir_all(&args.snaps_dir)?;
    for (entry, frame) in timing.iter().zip(frames.iter()) {
        let path = args.snaps_dir.join(format!("{}.json", entry.frame));
        let json = serde_json::to_vec(frame)?;
        std::fs::write(&path, json)?;
    }
    let timing_path = args.snaps_dir.join("timing.json");
    let timing_json = serde_json::to_vec_pretty(&timing)?;
    std::fs::write(&timing_path, timing_json)?;
    Ok(())
}

fn run_paint(args: &PaintArgs) -> anyhow::Result<()> {
    let snaps = list_numbered_snapshots(&args.snaps_dir)?;
    std::fs::create_dir_all(&args.frames_dir)?;
    let cfg = PaintConfig {
        font_size_px: args.font_size,
        padding_px: args.padding,
        cell_w_px: None,
        cell_h_px: None,
    };
    let painter = Painter::new(FONT_BYTES, cfg)?;
    snaps
        .par_iter()
        .try_for_each(|snap_path| -> anyhow::Result<()> {
            let frame = Frame::load(snap_path)?;
            let png_path = png_path_for(snap_path, &args.frames_dir)?;
            painter.save_png(&frame, &png_path)
        })?;
    Ok(())
}

fn run_encode(args: &EncodeArgs) -> anyhow::Result<()> {
    let bytes = std::fs::read(&args.timing)?;
    let timing: Vec<TimingEntry> = serde_json::from_slice(&bytes)?;
    encode(&EncodeRequest {
        frames_dir: args.frames_dir.clone(),
        timing,
        out_path: args.out.clone(),
        fps: args.fps,
        mp4_encoder: Mp4Encoder::Libx264,
        width: args.width,
    })
}

fn png_path_for(snap_path: &Path, frames_dir: &Path) -> anyhow::Result<PathBuf> {
    let stem = snap_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("snapshot path has no stem: {}", snap_path.display()))?;
    Ok(frames_dir.join(format!("{stem}.png")))
}
