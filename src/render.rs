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
use crate::receipt::{RECEIPT_VERSION, Receipt, RenderOptions, ToolIdentity, sha256_hex};
use crate::recorder::StubColors;
use crate::snapshot_replay::replay;

/// Builder for one cast → media render.
pub struct Render {
    cast: Cast,
    /// SHA-256 of the cast's raw bytes when loaded from a file.
    /// `None` for in-memory casts built via [`Render::new`]; receipt
    /// emission requires a hashed cast and will error otherwise.
    cast_sha256: Option<String>,
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
            cast_sha256: None,
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
        self.execute(out.as_ref())
    }

    /// Render to a media file and produce a [`Receipt`] describing
    /// the inputs, environment, and output hash.
    ///
    /// Requires the cast to have been loaded via [`render`] (so its
    /// hash is known). Calling on an in-memory `Render::new(cast)`
    /// errors.
    ///
    /// # Errors
    /// Same as [`Render::to_path`], plus: cast hash unknown (built
    /// via `Render::new` rather than `render(path)`); ffmpeg version
    /// query failed; output read-back failed.
    pub fn to_path_with_receipt(self, out: impl AsRef<Path>) -> anyhow::Result<Receipt> {
        let cast_sha256 = self.cast_sha256.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "to_path_with_receipt requires a cast loaded via term_recorder::render(path); \
                 Render::new(cast) does not track the source bytes"
            )
        })?;
        let render_opts = self.render_options();
        let out_path = out.as_ref().to_path_buf();

        // Capture tool identity BEFORE render so we don't pay an
        // ffmpeg fork twice when the render itself also forks ffmpeg.
        let tool = ToolIdentity::current()?;

        self.execute(&out_path)?;

        let output_bytes = std::fs::read(&out_path)?;
        let output_sha256 = sha256_hex(&output_bytes);
        let output_filename = out_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_string();

        Ok(Receipt {
            version: RECEIPT_VERSION,
            tool,
            cast_sha256,
            render: render_opts,
            output_sha256,
            output_filename,
        })
    }

    fn render_options(&self) -> RenderOptions {
        RenderOptions {
            font_size: self.font_size,
            padding: self.padding,
            width: self.width,
            fps: self.fps,
            mp4_encoder: match self.mp4_encoder {
                Mp4Encoder::Libx264 => "libx264".into(),
                Mp4Encoder::H264Nvenc => "h264_nvenc".into(),
            },
        }
    }

    fn execute(self, out: &Path) -> anyhow::Result<()> {
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
            out_path: out.to_path_buf(),
            fps: self.fps,
            mp4_encoder: self.mp4_encoder,
            width: self.width,
        })
    }
}

/// Headline API: read a cast file and prepare to render it.
///
/// The returned [`Render`] tracks the cast's content hash so callers
/// can later request a [`Receipt`] via [`Render::to_path_with_receipt`].
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
    let bytes = std::fs::read(&path)?;
    let cast_sha256 = sha256_hex(&bytes);
    let text = std::str::from_utf8(&bytes)?;
    let cast = Cast::parse(text)?;
    let mut r = Render::new(cast);
    r.cast_sha256 = Some(cast_sha256);
    Ok(r)
}
