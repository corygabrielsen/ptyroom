//! Headline rendering API: trace file -> MP4/GIF in one call.
//!
//! Wraps `frame_replay → paint → encode` so callers who don't need
//! the intermediate snapshot/PNG artifacts on disk can render in a
//! single chained expression. Intermediate frames live in a tempdir
//! for the duration of the call.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tempfile::TempDir;

use crate::encode::{EncodeRequest, Mp4Encoder, encode};
use crate::frame_replay::replay;
use crate::paint::{FONT_BYTES, PaintConfig, Painter};
use crate::witness::{RenderOptions, ToolIdentity, WITNESS_VERSION, Witness, sha256_hex};
use ptytrace::pty::StubColors;
use ptytrace::trace::Trace;

/// Builder for one trace -> media render.
pub struct Render {
    trace: Trace,
    /// SHA-256 of the trace's raw bytes when loaded from a file.
    /// `None` for in-memory traces built via [`Render::new`]; receipt
    /// emission requires a hashed trace and will error otherwise.
    trace_sha256: Option<String>,
    /// Optional behavioral contract hash. When set, the emitted
    /// receipt carries `contract_sha256: Some(...)` so verifiers know to
    /// require a spec.
    contract_sha256: Option<String>,
    /// Optional source-script provenance hash. When set, the emitted
    /// receipt carries `script_sha256: Some(...)` recording which
    /// `.script` file the input trace was produced from.
    script_sha256: Option<String>,
    /// Optional external provenance attestation hash. When set, the
    /// emitted receipt carries `attestation_sha256: Some(...)` so
    /// verifiers can require the matching attestation sidecar.
    attestation_sha256: Option<String>,
    font_size: f32,
    padding: u32,
    width: Option<u32>,
    fps: u32,
    stubs: StubColors,
    mp4_encoder: Mp4Encoder,
}

impl Render {
    /// Begin rendering an in-memory trace.
    #[must_use]
    pub fn new(trace: Trace) -> Self {
        Self {
            trace,
            trace_sha256: None,
            contract_sha256: None,
            script_sha256: None,
            attestation_sha256: None,
            font_size: 14.0,
            padding: 12,
            width: None,
            fps: 25,
            stubs: StubColors::default(),
            mp4_encoder: Mp4Encoder::Libx264,
        }
    }

    /// Pre-computed SHA-256 of a behavioral spec file. When set, the
    /// emitted receipt carries this hash so verifiers can require the
    /// matching spec via [`crate::witness::Witness::verify_with_spec`].
    #[must_use]
    pub fn contract_sha256(mut self, hash: impl Into<String>) -> Self {
        self.contract_sha256 = Some(hash.into());
        self
    }

    /// Pre-computed SHA-256 of the source `.script` file. When set, the
    /// emitted receipt records this as provenance, so third parties can
    /// confirm a held script file is byte-identical to the recipe that
    /// produced this trace.
    #[must_use]
    pub fn script_sha256(mut self, hash: impl Into<String>) -> Self {
        self.script_sha256 = Some(hash.into());
        self
    }

    /// Pre-computed SHA-256 of an attestation file. When set, the
    /// emitted receipt records this as external provenance — verifiers
    /// can require the matching attestation sidecar and confirm it
    /// targets this trace.
    #[must_use]
    pub fn attestation_sha256(mut self, hash: impl Into<String>) -> Self {
        self.attestation_sha256 = Some(hash.into());
        self
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
    /// Frame replay failed, paint failed, or ffmpeg invocation
    /// returned non-zero.
    pub fn to_path(self, out: impl AsRef<Path>) -> anyhow::Result<()> {
        self.execute(out.as_ref())
    }

    /// Render to a media file and produce a [`Witness`] describing
    /// the inputs, environment, and output hash.
    ///
    /// Requires the trace to have been loaded via [`render`] (so its
    /// hash is known). Calling on an in-memory `Render::new(trace)`
    /// errors.
    ///
    /// ```no_run
    /// let receipt = ptyrender::render("demo.ptytrace")?
    ///     .font_size(40.0)
    ///     .to_path_with_receipt("demo.gif")?;
    /// receipt.write("demo.gif.receipt.json")?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    ///
    /// # Errors
    /// Same as [`Render::to_path`], plus: trace hash unknown (built
    /// via `Render::new` rather than `render(path)`); ffmpeg version
    /// query failed; output read-back failed.
    pub fn to_path_with_receipt(self, out: impl AsRef<Path>) -> anyhow::Result<Witness> {
        let trace_sha256 = self.trace_sha256.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "to_path_with_receipt requires a trace loaded via ptyrender::render(path); \
                 Render::new(trace) does not track the source bytes"
            )
        })?;
        let contract_sha256 = self.contract_sha256.clone();
        let script_sha256 = self.script_sha256.clone();
        let attestation_sha256 = self.attestation_sha256.clone();
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

        Ok(Witness {
            version: WITNESS_VERSION,
            tool,
            trace_sha256,
            render: render_opts,
            output_sha256,
            output_filename,
            contract_sha256,
            script_sha256,
            attestation_sha256,
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

        let (snapshots, timing) = replay(&self.trace, self.stubs)?;
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

/// Headline API: read a trace file and prepare to render it.
///
/// The returned [`Render`] tracks the trace's content hash so callers
/// can later request a [`Witness`] via [`Render::to_path_with_receipt`].
///
/// ```no_run
/// ptyrender::render("demo.ptytrace")?.to_path("demo.mp4")?;
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
/// Trace file missing, unreadable, or has a malformed header.
pub fn render(trace: impl AsRef<Path>) -> anyhow::Result<Render> {
    let path: PathBuf = trace.as_ref().to_path_buf();
    let bytes = std::fs::read(&path)?;
    let trace_sha256 = sha256_hex(&bytes);
    let text = std::str::from_utf8(&bytes)?;
    let trace = Trace::parse(text)?;
    let mut r = Render::new(trace);
    r.trace_sha256 = Some(trace_sha256);
    Ok(r)
}
