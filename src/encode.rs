//! GIF encoder: PNG sequence + per-frame timing → animated GIF via ffmpeg.
//!
//! ffmpeg is invoked once with the concat demuxer reading a generated
//! `concat.txt` listing each frame and its `duration` directive. The
//! demuxer requires the last frame to be repeated (its `duration` is
//! ignored); we mirror that quirk here.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// One entry of the `timing.json` written by `snapshot.ts` (and its Rust
/// successor).
#[derive(Debug, Clone, Deserialize)]
pub struct TimingEntry {
    pub frame: String,
    pub dwell_ms: u32,
}

impl TimingEntry {
    #[must_use] 
    pub fn dwell_seconds(&self) -> f64 {
        f64::from(self.dwell_ms) / 1000.0
    }
}

#[derive(Debug, Clone)]
pub struct EncodeRequest {
    pub frames_dir: PathBuf,
    pub timing: Vec<TimingEntry>,
    pub out_gif: PathBuf,
    pub fps: u32,
    /// Optional output width in pixels. When set, ffmpeg's lanczos scale
    /// filter resizes frames to this width preserving aspect ratio
    /// (height auto-computed). Used to render a single high-resolution
    /// frame set into multiple output sizes (e.g. paint at `FONT_SIZE=28`
    /// once, encode native MP4 + scaled-down GIF for the README).
    pub width: Option<u32>,
}

/// Render the GIF. Returns `Ok(())` on ffmpeg success; the caller prints
/// the command before invoking so failures can be reproduced.
///
/// # Errors
/// Empty timing list, missing PNG frame, IO error writing the concat file,
/// or non-zero exit status from ffmpeg.
pub fn encode(req: &EncodeRequest) -> anyhow::Result<()> {
    if req.timing.is_empty() {
        anyhow::bail!("encode: timing has no frames");
    }

    // Canonicalize frames_dir so the concat file's `file '...'` entries
    // are absolute paths. ffmpeg's concat demuxer resolves relative
    // entries against the concat file's directory, not the current
    // working directory — making any relative input path order-fragile
    // depending on whether we're running inside the container (where
    // cwd is /work and frames_dir is "/work/frames") or on the host
    // (where cwd is the project root and frames_dir is "assets/frames").
    let frames_dir = req.frames_dir.canonicalize()
        .map_err(|e| anyhow::anyhow!("frames_dir does not exist or is unreadable: {}: {e}",
                                      req.frames_dir.display()))?;

    let concat_path = frames_dir.parent()
        .ok_or_else(|| anyhow::anyhow!("frames_dir has no parent: {}", frames_dir.display()))?
        .join("concat.txt");

    let concat_text = build_concat(&frames_dir, &req.timing)?;
    std::fs::write(&concat_path, concat_text)?;

    let ext = req.out_gif.extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("gif") => encode_gif(req, &concat_path),
        Some("mp4") => encode_mp4(req, &concat_path),
        Some(other) => anyhow::bail!("encode: unsupported output extension '.{other}' (expected .gif or .mp4)"),
        None => anyhow::bail!("encode: output path has no extension: {}", req.out_gif.display()),
    }
}

fn encode_gif(req: &EncodeRequest, concat_path: &Path) -> anyhow::Result<()> {
    use std::fmt::Write as _;

    let mut filter = String::new();
    write!(filter, "fps={fps}", fps = req.fps)?;
    if let Some(w) = req.width {
        // -2 keeps the aspect ratio and rounds height to an even number
        // (required by yuv420p; harmless for GIF).
        write!(filter, ",scale={w}:-2:flags=lanczos")?;
    }
    write!(
        filter,
        ",split[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse=dither=bayer:bayer_scale=5",
    )?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-f", "concat", "-safe", "0", "-i"])
       .arg(concat_path)
       .args(["-vf", &filter, "-loop", "0"])
       .arg(&req.out_gif);
    run_ffmpeg(&mut cmd)
}

/// H.264 MP4 with browser-friendly defaults: yuv420p (universal compat),
/// faststart (moov atom upfront for progressive playback), crf 20 (visually
/// lossless for terminal content while staying small — typically <500KB).
///
/// Pacing knobs:
/// - `-preset medium` instead of slow: terminal content is mostly static
///   text, so the slower presets buy little quality but cost ~30% more
///   wall time. Medium is the libx264 default and produces visually
///   indistinguishable output for our content.
/// - `-tune stillimage` biases the encoder toward static-image content,
///   which matches a screen-recording workload (long stretches of
///   identical frames).
fn encode_mp4(req: &EncodeRequest, concat_path: &Path) -> anyhow::Result<()> {
    use std::fmt::Write as _;

    let mut filter = String::new();
    write!(filter, "fps={fps}", fps = req.fps)?;
    if let Some(w) = req.width {
        write!(filter, ",scale={w}:-2:flags=lanczos")?;
    }
    write!(filter, ",format=yuv420p")?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-f", "concat", "-safe", "0", "-i"])
       .arg(concat_path)
       .args(["-vf", &filter])
       .args(["-c:v", "libx264", "-crf", "20", "-preset", "medium", "-tune", "stillimage"])
       .args(["-profile:v", "high", "-level", "4.0"])
       .args(["-movflags", "+faststart"])
       .arg(&req.out_gif);
    run_ffmpeg(&mut cmd)
}

fn run_ffmpeg(cmd: &mut Command) -> anyhow::Result<()> {
    eprintln!("$ {cmd:?}");
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("ffmpeg exited with status {status}");
    }
    Ok(())
}

fn build_concat(frames_dir: &Path, timing: &[TimingEntry]) -> anyhow::Result<String> {
    use std::fmt::Write as _;

    let mut s = String::new();
    for entry in timing {
        let png = frames_dir.join(format!("{}.png", entry.frame));
        if !png.exists() {
            anyhow::bail!("missing frame PNG: {}", png.display());
        }
        writeln!(s, "file '{}'", png.display())?;
        writeln!(s, "duration {:.4}", entry.dwell_seconds())?;
    }
    // ffmpeg concat demuxer quirk: last frame is repeated, its duration ignored.
    let last = &timing[timing.len() - 1];
    let last_png = frames_dir.join(format!("{}.png", last.frame));
    writeln!(s, "file '{}'", last_png.display())?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn timing_entry_dwell_seconds() {
        let e = TimingEntry { frame: "0001".into(), dwell_ms: 250 };
        assert!((e.dwell_seconds() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn build_concat_emits_repeated_last_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let frames = tmp.path().join("frames");
        fs::create_dir_all(&frames).unwrap();
        for n in &["0001", "0002"] {
            let mut f = fs::File::create(frames.join(format!("{n}.png"))).unwrap();
            f.write_all(b"fake-png").unwrap();
        }
        let timing = vec![
            TimingEntry { frame: "0001".into(), dwell_ms: 100 },
            TimingEntry { frame: "0002".into(), dwell_ms: 200 },
        ];
        let s = build_concat(&frames, &timing).unwrap();
        // Two real frames + one repeated trailer = 3 `file` lines, 2 `duration` lines.
        assert_eq!(s.matches("file '").count(), 3);
        assert_eq!(s.matches("duration ").count(), 2);
        assert!(s.contains("duration 0.1000"));
        assert!(s.contains("duration 0.2000"));
    }

    #[test]
    fn build_concat_errors_on_missing_png() {
        let tmp = tempfile::tempdir().unwrap();
        let frames = tmp.path().join("frames");
        fs::create_dir_all(&frames).unwrap();
        let timing = vec![TimingEntry { frame: "0001".into(), dwell_ms: 100 }];
        assert!(build_concat(&frames, &timing).is_err());
    }
}
