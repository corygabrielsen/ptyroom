//! Headline rendering API: cast file → MP4/GIF in one call.
//!
//! Wraps `snapshot_replay → paint → encode` so callers who don't need
//! the intermediate snapshot/PNG artifacts on disk can render in a
//! single chained expression. Intermediate frames live in a tempdir
//! for the duration of the call.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tempfile::TempDir;

use crate::cast::Cast;
use crate::encode::{EncodeRequest, Mp4Encoder, encode};
use crate::paint::{FONT_BYTES, PaintConfig, Painter};
use crate::recorder::StubColors;
use crate::snapshot_replay::replay;

/// Builder for one cast → media render.
pub struct Render {
    cast: Cast,
    font_size: f32,
    padding: u32,
    width: Option<u32>,
    fps: u32,
    stubs: StubColors,
    mp4_encoder: Mp4Encoder,
}

impl Render {
    /// Begin rendering an in-memory cast.
    #[must_use]
    pub fn new(cast: Cast) -> Self {
        Self {
            cast,
            font_size: 14.0,
            padding: 12,
            width: None,
            fps: 25,
            stubs: StubColors::default(),
            mp4_encoder: Mp4Encoder::Libx264,
        }
    }

    /// Font size in pixels (default `14.0`).
    #[must_use]
    pub fn font_size(mut self, sz: f32) -> Self {
        self.font_size = sz;
        self
    }

    /// Padding around the grid in pixels (default `12`).
    #[must_use]
    pub fn padding(mut self, px: u32) -> Self {
        self.padding = px;
        self
    }

    /// Output width in pixels. When set, frames are scaled (lanczos).
    /// Height is auto-computed to preserve aspect ratio.
    #[must_use]
    pub fn width(mut self, px: u32) -> Self {
        self.width = Some(px);
        self
    }

    /// Frame rate (default `25`).
    #[must_use]
    pub fn fps(mut self, fps: u32) -> Self {
        self.fps = fps;
        self
    }

    /// Override the OSC 10/11 stub responses used during snapshot replay.
    #[must_use]
    pub fn stubs(mut self, stubs: StubColors) -> Self {
        self.stubs = stubs;
        self
    }

    /// MP4 encoder choice (only used when the output path ends in `.mp4`).
    #[must_use]
    pub fn mp4_encoder(mut self, e: Mp4Encoder) -> Self {
        self.mp4_encoder = e;
        self
    }

    /// Render to a media file. Format is inferred from the path
    /// extension (`.mp4` or `.gif`).
    ///
    /// # Errors
    /// Snapshot replay failed, paint failed, or ffmpeg invocation
    /// returned non-zero.
    pub fn to_path(self, out: impl AsRef<Path>) -> anyhow::Result<()> {
        let work = TempDir::new()?;
        let frames_dir = work.path().join("frames");
        std::fs::create_dir(&frames_dir)?;

        let (snapshots, timing) = replay(&self.cast, self.stubs)?;
        let painter = Painter::new(
            FONT_BYTES,
            PaintConfig {
                font_size_px: self.font_size,
                padding_px: self.padding,
                cell_w_px: None,
                cell_h_px: None,
            },
        )?;

        snapshots
            .par_iter()
            .zip(&timing)
            .try_for_each(|(snap, entry)| -> anyhow::Result<()> {
                let png_path = frames_dir.join(format!("{}.png", entry.frame));
                painter.save_png(snap, &png_path)?;
                Ok(())
            })?;

        encode(&EncodeRequest {
            frames_dir,
            timing,
            out_path: out.as_ref().to_path_buf(),
            fps: self.fps,
            mp4_encoder: self.mp4_encoder,
            width: self.width,
        })
    }
}

/// Headline API: read a cast file and prepare to render it.
///
/// ```no_run
/// term_recorder::render("demo.cast")?.to_path("demo.mp4")?;
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
/// Cast file missing, unreadable, or has a malformed header.
pub fn render(cast: impl AsRef<Path>) -> anyhow::Result<Render> {
    let path: PathBuf = cast.as_ref().to_path_buf();
    let cast = Cast::read(&path)?;
    Ok(Render::new(cast))
}
