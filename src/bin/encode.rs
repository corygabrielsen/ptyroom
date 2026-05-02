//! CLI: PNG sequence + timing.json → animated GIF.
use std::path::PathBuf;

use clap::Parser;
use tint_recorder::encode::{EncodeRequest, TimingEntry, encode};

/// Encode a PNG sequence into a GIF.
///
/// Reads `timing.json` (a list of `{frame, dwell_ms}` entries written by the
/// snapshot stage) and emits a single GIF via ffmpeg's concat demuxer. The
/// timing values come from the recorded scene's intent (`dwell_ms`), so
/// playback is independent of the wall-clock time of the recording.
#[derive(Parser)]
struct Args {
    /// Directory containing the PNG frames.
    frames_dir: PathBuf,
    /// Path to `timing.json` written by the snapshot stage.
    timing_json: PathBuf,
    /// Output GIF path.
    out_gif: PathBuf,
    /// Output GIF frame rate.
    #[arg(long, default_value_t = 25)]
    fps: u32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let timing_bytes = std::fs::read(&args.timing_json)?;
    let timing: Vec<TimingEntry> = serde_json::from_slice(&timing_bytes)?;
    encode(&EncodeRequest {
        frames_dir: args.frames_dir,
        timing,
        out_gif: args.out_gif,
        fps: args.fps,
    })?;
    Ok(())
}
