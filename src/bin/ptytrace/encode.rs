//! `encode` subcommand: PNG sequence + timing.json → GIF/MP4.

use std::path::PathBuf;

use clap::ValueEnum;
use ptytrace::encode::{EncodeRequest, Mp4Encoder, TimingEntry, encode};

#[derive(clap::Args)]
pub struct Args {
    /// Directory containing the PNG frames.
    frames_dir: PathBuf,
    /// Path to `timing.json` written by the snapshot stage.
    timing_json: PathBuf,
    /// Output path. Format is detected from the extension (.gif or .mp4).
    out_path: PathBuf,
    /// Output frame rate.
    #[arg(long, default_value_t = 25)]
    fps: u32,
    /// Optional output width in pixels. When set, frames are scaled
    /// (lanczos) to this width with height auto-computed to preserve
    /// aspect ratio.
    #[arg(long)]
    width: Option<u32>,
    /// MP4 encoder to use when the output path ends in `.mp4`.
    #[arg(long, value_enum, default_value = "libx264")]
    mp4_encoder: Mp4EncoderArg,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mp4EncoderArg {
    Libx264,
    #[value(name = "h264_nvenc", alias = "h264-nvenc")]
    H264Nvenc,
}

impl From<Mp4EncoderArg> for Mp4Encoder {
    fn from(value: Mp4EncoderArg) -> Self {
        match value {
            Mp4EncoderArg::Libx264 => Self::Libx264,
            Mp4EncoderArg::H264Nvenc => Self::H264Nvenc,
        }
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let timing_bytes = std::fs::read(&args.timing_json)?;
    let timing: Vec<TimingEntry> = serde_json::from_slice(&timing_bytes)?;
    encode(&EncodeRequest {
        frames_dir: args.frames_dir,
        timing,
        out_path: args.out_path,
        fps: args.fps,
        mp4_encoder: args.mp4_encoder.into(),
        width: args.width,
    })
}
