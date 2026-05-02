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
}

/// Render the GIF. Returns `Ok(())` on ffmpeg success; the caller prints
/// the command before invoking so failures can be reproduced.
pub fn encode(req: &EncodeRequest) -> anyhow::Result<()> {
    if req.timing.is_empty() {
        anyhow::bail!("encode: timing has no frames");
    }

    let concat_path = req.frames_dir.parent()
        .ok_or_else(|| anyhow::anyhow!("frames_dir has no parent: {:?}", req.frames_dir))?
        .join("concat.txt");

    let concat_text = build_concat(&req.frames_dir, &req.timing)?;
    std::fs::write(&concat_path, concat_text)?;

    let filter = format!(
        "fps={fps},split[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse=dither=bayer:bayer_scale=5",
        fps = req.fps,
    );
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-f", "concat", "-safe", "0", "-i"])
       .arg(&concat_path)
       .args(["-vf", &filter, "-loop", "0"])
       .arg(&req.out_gif);
    eprintln!("$ {cmd:?}");
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("ffmpeg exited with status {status}");
    }
    Ok(())
}

fn build_concat(frames_dir: &Path, timing: &[TimingEntry]) -> anyhow::Result<String> {
    let mut s = String::new();
    for entry in timing {
        let png = frames_dir.join(format!("{}.png", entry.frame));
        if !png.exists() {
            anyhow::bail!("missing frame PNG: {}", png.display());
        }
        s.push_str(&format!("file '{}'\n", png.display()));
        s.push_str(&format!("duration {:.4}\n", entry.dwell_seconds()));
    }
    // ffmpeg concat demuxer quirk: last frame is repeated, its duration ignored.
    let last = &timing[timing.len() - 1];
    let last_png = frames_dir.join(format!("{}.png", last.frame));
    s.push_str(&format!("file '{}'\n", last_png.display()));
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
