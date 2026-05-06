//! Reproducibility receipts for rendered casts.
//!
//! A [`Receipt`] is a JSON sidecar that lets a third party verify the
//! rendered output (MP4/GIF) was produced from a known cast file by a
//! known pipeline. The receipt is written alongside the artifact and
//! verified later by [`Receipt::verify`], which re-runs the pipeline
//! with the recorded inputs and confirms the output bytes hash to the
//! same value.
//!
//! This is the nix/sigstore-shaped layer on top of the deterministic
//! render pipeline — it exposes the determinism as an externally
//! checkable property rather than an internal type-state.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cast::Cast;
use crate::encode::Mp4Encoder;
use crate::paint::FONT_BYTES;
use crate::render::Render;

/// Current schema version. Bump on breaking changes.
pub const RECEIPT_VERSION: u32 = 1;

/// On-disk reproducibility receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Schema version; must equal [`RECEIPT_VERSION`].
    pub version: u32,
    /// Tool / environment identity at production time.
    pub tool: ToolIdentity,
    /// SHA-256 of the input cast file (raw bytes).
    pub cast_sha256: String,
    /// Render configuration that produced the output.
    pub render: RenderOptions,
    /// SHA-256 of the produced output bytes.
    pub output_sha256: String,
    /// Output filename at production time (informational).
    pub output_filename: String,
}

/// Pipeline identity — the parts of the environment that affect
/// byte-stable output. Two machines with identical [`ToolIdentity`]
/// fields should produce identical render output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub name: String,
    pub version: String,
    pub ffmpeg_version: String,
    pub font_sha256: String,
}

impl ToolIdentity {
    /// Capture the current environment's identity. Queries `ffmpeg
    /// -version` and hashes the bundled font.
    ///
    /// # Errors
    /// `ffmpeg` not on PATH or returned non-zero.
    pub fn current() -> anyhow::Result<Self> {
        Ok(Self {
            name: "term-recorder".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ffmpeg_version: detect_ffmpeg_version()?,
            font_sha256: sha256_hex(FONT_BYTES),
        })
    }
}

/// Render knobs that affect output bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RenderOptions {
    pub font_size: f32,
    pub padding: u32,
    pub width: Option<u32>,
    pub fps: u32,
    pub mp4_encoder: String,
}

impl RenderOptions {
    /// Parse the encoder string back into the typed enum.
    ///
    /// # Errors
    /// Encoder string not recognized.
    pub fn parsed_mp4_encoder(&self) -> anyhow::Result<Mp4Encoder> {
        match self.mp4_encoder.as_str() {
            "libx264" => Ok(Mp4Encoder::Libx264),
            "h264_nvenc" => Ok(Mp4Encoder::H264Nvenc),
            other => anyhow::bail!("receipt: unknown mp4_encoder '{other}'"),
        }
    }
}

/// Outcome of a [`Receipt::verify`] call.
#[derive(Debug, Clone)]
pub enum VerifyOutcome {
    /// Cast hash, environment, and re-rendered output all match.
    Match,
    /// Provided cast file's hash differs from the receipt's claim.
    CastDiffers { expected: String, got: String },
    /// Local environment differs from the receipt's pipeline identity.
    /// `field` names which sub-field disagreed.
    EnvironmentDiffers {
        field: String,
        expected: String,
        got: String,
    },
    /// Re-rendered output bytes hash to a different value.
    OutputDiffers { expected: String, got: String },
}

impl VerifyOutcome {
    #[must_use]
    pub fn is_match(&self) -> bool {
        matches!(self, VerifyOutcome::Match)
    }
}

impl std::fmt::Display for VerifyOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Match => write!(f, "MATCH"),
            Self::CastDiffers { expected, got } => {
                write!(f, "CAST_DIFFERS expected={expected} got={got}")
            }
            Self::EnvironmentDiffers {
                field,
                expected,
                got,
            } => write!(
                f,
                "ENV_DIFFERS field={field} expected={expected:?} got={got:?}"
            ),
            Self::OutputDiffers { expected, got } => {
                write!(f, "OUTPUT_DIFFERS expected={expected} got={got}")
            }
        }
    }
}

impl Receipt {
    /// Read a receipt from disk.
    ///
    /// # Errors
    /// IO error or JSON parse failure.
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path.as_ref()).context("read receipt")?;
        let receipt: Self = serde_json::from_slice(&bytes).context("parse receipt")?;
        anyhow::ensure!(
            receipt.version == RECEIPT_VERSION,
            "receipt version {} not supported (expected {})",
            receipt.version,
            RECEIPT_VERSION,
        );
        Ok(receipt)
    }

    /// Write the receipt to disk as pretty-printed JSON with a
    /// trailing newline.
    ///
    /// # Errors
    /// IO error or serialization failure.
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        std::fs::write(path.as_ref(), json).context("write receipt")?;
        Ok(())
    }

    /// Re-render the cast and confirm every claim in the receipt
    /// holds. Returns the structured outcome instead of erroring on
    /// mismatch — callers decide how to react.
    ///
    /// # Errors
    /// Cast file missing, ffmpeg invocation failed, or other IO error
    /// during re-render. Receipt-claim mismatches return
    /// `Ok(VerifyOutcome::*)`, not `Err`.
    pub fn verify(&self, cast_path: impl AsRef<Path>) -> anyhow::Result<VerifyOutcome> {
        let cast_path = cast_path.as_ref();

        // 1. Cast hash check. If the verifier was handed a different
        //    cast file than the one the receipt was produced from, we
        //    can't reproduce anything meaningful.
        let cast_bytes = std::fs::read(cast_path).context("read cast for verify")?;
        let cast_hash = sha256_hex(&cast_bytes);
        if cast_hash != self.cast_sha256 {
            return Ok(VerifyOutcome::CastDiffers {
                expected: self.cast_sha256.clone(),
                got: cast_hash,
            });
        }

        // 2. Environment check. Different ffmpeg / font / tool version
        //    means the pipeline identity differs; even bit-exact inputs
        //    won't reproduce.
        let current = ToolIdentity::current()?;
        if let Some(diff) = first_env_diff(&self.tool, &current) {
            return Ok(diff);
        }

        // 3. Re-render the cast with the receipt's config into a
        //    tempfile sized by the recorded extension, then hash and
        //    compare against the receipt's output_sha256.
        let ext = Path::new(&self.output_filename)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "receipt: output_filename {:?} has no extension",
                    self.output_filename
                )
            })?;
        let tmp = tempfile::Builder::new()
            .prefix("term-recorder-verify-")
            .suffix(&format!(".{ext}"))
            .tempfile()
            .context("create verify tempfile")?;

        let cast = Cast::parse(std::str::from_utf8(&cast_bytes)?)?;
        let mp4_encoder = self.render.parsed_mp4_encoder()?;
        let mut r = Render::new(cast)
            .font_size(self.render.font_size)
            .padding(self.render.padding)
            .fps(self.render.fps)
            .mp4_encoder(mp4_encoder);
        if let Some(w) = self.render.width {
            r = r.width(w);
        }
        r.to_path(tmp.path())?;

        let output_bytes = std::fs::read(tmp.path()).context("read re-rendered output")?;
        let output_hash = sha256_hex(&output_bytes);
        if output_hash != self.output_sha256 {
            return Ok(VerifyOutcome::OutputDiffers {
                expected: self.output_sha256.clone(),
                got: output_hash,
            });
        }

        Ok(VerifyOutcome::Match)
    }
}

fn first_env_diff(expected: &ToolIdentity, got: &ToolIdentity) -> Option<VerifyOutcome> {
    let pairs = [
        ("tool.name", &expected.name, &got.name),
        ("tool.version", &expected.version, &got.version),
        (
            "tool.ffmpeg_version",
            &expected.ffmpeg_version,
            &got.ffmpeg_version,
        ),
        ("tool.font_sha256", &expected.font_sha256, &got.font_sha256),
    ];
    for (field, exp, cur) in pairs {
        if exp != cur {
            return Some(VerifyOutcome::EnvironmentDiffers {
                field: (*field).to_string(),
                expected: (*exp).clone(),
                got: (*cur).clone(),
            });
        }
    }
    None
}

/// Hex-encoded SHA-256 of a byte slice.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        write!(&mut s, "{b:02x}").expect("infallible String fmt");
    }
    s
}

fn detect_ffmpeg_version() -> anyhow::Result<String> {
    let out = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .context("invoke ffmpeg -version")?;
    if !out.status.success() {
        anyhow::bail!("ffmpeg -version exited {}", out.status);
    }
    let stdout = std::str::from_utf8(&out.stdout)?;
    // First line: "ffmpeg version 6.1.1 ..."; record verbatim.
    let line = stdout.lines().next().unwrap_or("").trim();
    Ok(line.to_string())
}
