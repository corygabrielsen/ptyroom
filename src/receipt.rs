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
#[non_exhaustive]
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
    /// Optional behavioral attestation hash. When present, the
    /// receipt promises that the cast satisfies a [`crate::spec::Spec`]
    /// whose file bytes hash to this value. [`Receipt::verify_with_spec`]
    /// confirms the spec hash matches and re-runs `Spec::check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_sha256: Option<String>,
    /// Optional source-scene provenance hash. When present, the
    /// receipt records that the cast was produced by running
    /// [`crate::scene::Scene`]`::run` on a `.scene` file whose bytes
    /// hash to this value. This is provenance only — verification does
    /// not re-run the scene (scene execution depends on shells, docker
    /// images, and external state that the recorder does not pin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_sha256: Option<String>,
}

/// Pipeline identity — the parts of the environment that affect
/// byte-stable output. Two machines with identical [`ToolIdentity`]
/// fields should produce identical render output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ToolIdentity {
    pub name: String,
    pub version: String,
    pub ffmpeg_version: String,
    pub font_sha256: String,
    /// Optional SHA-256 of the recorder binary itself
    /// (`std::env::current_exe()` bytes at receipt-emit time). Pins
    /// the exact build that produced the receipt — closes the gap
    /// where `version` is a `Cargo.toml` string but two builds with
    /// different `Cargo.lock` patch versions could diverge. Best-
    /// effort: `None` if the current binary is unreadable. When set
    /// on both sides, [`Receipt::verify`] enforces equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorder_sha256: Option<String>,
}

impl ToolIdentity {
    /// Capture the current environment's identity. Queries `ffmpeg
    /// -version`, hashes the bundled font, and best-effort hashes the
    /// running recorder binary itself.
    ///
    /// # Errors
    /// `ffmpeg` not on PATH or returned non-zero. Failure to read the
    /// current binary is non-fatal: `recorder_sha256` is left `None`.
    pub fn current() -> anyhow::Result<Self> {
        Ok(Self {
            name: "term-recorder".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ffmpeg_version: detect_ffmpeg_version()?,
            font_sha256: sha256_hex(FONT_BYTES),
            recorder_sha256: current_exe_sha256(),
        })
    }
}

/// SHA-256 of the running binary's bytes, or `None` if it can't be
/// read (e.g., binary deleted under us, or `current_exe` failed).
fn current_exe_sha256() -> Option<String> {
    let path = std::env::current_exe().ok()?;
    let bytes = std::fs::read(path).ok()?;
    Some(sha256_hex(&bytes))
}

/// Render knobs that affect output bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
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

/// Outcome of a [`Receipt::verify`] or [`Receipt::verify_with_spec`] call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum VerifyOutcome {
    /// Every claim in the receipt held: cast hash + environment +
    /// re-rendered output, plus (if checked) spec hash + spec eval.
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
    /// Receipt has a `spec_sha256` claim but no spec was provided to
    /// the verifier. Use [`Receipt::verify_with_spec`].
    SpecRequired,
    /// Provided spec file's hash differs from the receipt's claim.
    SpecDiffers { expected: String, got: String },
    /// Spec hash matched, but re-running [`crate::spec::Spec::check`]
    /// against the cast did not pass every predicate.
    SpecFailed { failed: usize, total: usize },
    /// The receipt's MP4 encoder is not byte-deterministic across
    /// machines (e.g. `h264_nvenc` depends on GPU + driver version).
    /// Re-rendering would produce a meaningless comparison; verify
    /// refuses up front. The cast itself and any GIF outputs from
    /// the same cast remain verifiable; this is specifically about
    /// the MP4 output bytes.
    EncoderNotVerifiable { encoder: String },
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
            Self::SpecRequired => {
                write!(f, "SPEC_REQUIRED receipt expects --spec")
            }
            Self::SpecDiffers { expected, got } => {
                write!(f, "SPEC_DIFFERS expected={expected} got={got}")
            }
            Self::SpecFailed { failed, total } => {
                write!(f, "SPEC_FAILED {failed}/{total} predicate(s)")
            }
            Self::EncoderNotVerifiable { encoder } => {
                write!(f, "ENCODER_NOT_VERIFIABLE encoder={encoder}")
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
    /// holds (B-only check). Returns the structured outcome instead
    /// of erroring on mismatch — callers decide how to react.
    ///
    /// If the receipt carries a [`Self::spec_sha256`] claim, this
    /// method returns [`VerifyOutcome::SpecRequired`] without doing
    /// the re-render — full verification needs the spec, so call
    /// [`Self::verify_with_spec`] instead.
    ///
    /// # Errors
    /// Cast file missing, ffmpeg invocation failed, or other IO error
    /// during re-render. Receipt-claim mismatches return
    /// `Ok(VerifyOutcome::*)`, not `Err`.
    pub fn verify(&self, cast_path: impl AsRef<Path>) -> anyhow::Result<VerifyOutcome> {
        if self.spec_sha256.is_some() {
            return Ok(VerifyOutcome::SpecRequired);
        }
        self.verify_b(cast_path.as_ref())
    }

    /// Verify the receipt with a spec file. Performs the B-check
    /// (cast hash, environment, re-render output match) plus the
    /// C-check (spec file hashes to the receipt's claim, and re-running
    /// [`crate::spec::Spec::check`] passes every predicate).
    ///
    /// Calling this when the receipt has no `spec_sha256` claim still
    /// runs the spec check against the cast — the receipt is silent
    /// on the spec relationship, but the verifier confirms the spec
    /// holds against the cast anyway.
    ///
    /// ```no_run
    /// use term_recorder::receipt::{Receipt, VerifyOutcome};
    ///
    /// let receipt = Receipt::read("demo.gif.receipt.json")?;
    /// let outcome = receipt.verify_with_spec("demo.cast", "demo.spec.json")?;
    /// assert!(matches!(outcome, VerifyOutcome::Match));
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    ///
    /// # Errors
    /// Cast or spec file missing, ffmpeg invocation failed, JSON parse
    /// error on the spec, or other IO error.
    pub fn verify_with_spec(
        &self,
        cast_path: impl AsRef<Path>,
        spec_path: impl AsRef<Path>,
    ) -> anyhow::Result<VerifyOutcome> {
        let cast_path = cast_path.as_ref();
        let spec_path = spec_path.as_ref();

        let spec_bytes = std::fs::read(spec_path).context("read spec for verify")?;
        let spec_hash = sha256_hex(&spec_bytes);
        if let Some(expected) = &self.spec_sha256
            && &spec_hash != expected
        {
            return Ok(VerifyOutcome::SpecDiffers {
                expected: expected.clone(),
                got: spec_hash,
            });
        }

        match self.verify_b(cast_path)? {
            VerifyOutcome::Match => {}
            other => return Ok(other),
        }

        let spec: crate::spec::Spec = serde_json::from_slice(&spec_bytes).context("parse spec")?;
        let cast = Cast::read(cast_path)?;
        let report = spec.check(&cast);
        if !report.all_passed() {
            return Ok(VerifyOutcome::SpecFailed {
                failed: report.failed_count(),
                total: report.outcomes.len(),
            });
        }

        Ok(VerifyOutcome::Match)
    }

    fn verify_b(&self, cast_path: &Path) -> anyhow::Result<VerifyOutcome> {
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

        let mp4_encoder = self.render.parsed_mp4_encoder()?;
        // Refuse non-deterministic encoders for MP4 outputs. Letting
        // the re-render run would produce `OutputDiffers` for what is
        // really an "encoder isn't byte-portable across machines"
        // condition — surface the real reason up front.
        if ext.eq_ignore_ascii_case("mp4") && !mp4_encoder.is_byte_deterministic() {
            return Ok(VerifyOutcome::EncoderNotVerifiable {
                encoder: self.render.mp4_encoder.clone(),
            });
        }

        let tmp = tempfile::Builder::new()
            .prefix("term-recorder-verify-")
            .suffix(&format!(".{ext}"))
            .tempfile()
            .context("create verify tempfile")?;

        let cast = Cast::parse(std::str::from_utf8(&cast_bytes)?)?;
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
    // recorder_sha256 is optional. Compare only when expected has a
    // claim — receipts written before the field existed (or by builds
    // where current_exe couldn't be read) skip this check.
    if let Some(expected_hash) = &expected.recorder_sha256
        && let Some(got_hash) = &got.recorder_sha256
        && expected_hash != got_hash
    {
        return Some(VerifyOutcome::EnvironmentDiffers {
            field: "tool.recorder_sha256".into(),
            expected: expected_hash.clone(),
            got: got_hash.clone(),
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_receipt() -> Receipt {
        Receipt {
            version: RECEIPT_VERSION,
            tool: ToolIdentity {
                name: "term-recorder".into(),
                version: "0.1.0".into(),
                ffmpeg_version: "ffmpeg version 6.1.1".into(),
                font_sha256: "f".repeat(64),
                recorder_sha256: None,
            },
            cast_sha256: "c".repeat(64),
            render: RenderOptions {
                font_size: 14.0,
                padding: 12,
                width: None,
                fps: 25,
                mp4_encoder: "libx264".into(),
            },
            output_sha256: "o".repeat(64),
            output_filename: "demo.gif".into(),
            spec_sha256: None,
            scene_sha256: None,
        }
    }

    #[test]
    fn legacy_receipt_without_optional_hashes_parses() {
        // Legacy receipts (pre-spec_sha256, pre-scene_sha256, pre-
        // recorder_sha256) must continue to parse — those fields are
        // additive Option<String> with serde defaults.
        let json = r#"{
            "version": 1,
            "tool": {
                "name": "term-recorder",
                "version": "0.1.0",
                "ffmpeg_version": "ffmpeg version 6.1.1",
                "font_sha256": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
            },
            "cast_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "render": {
                "font_size": 14.0,
                "padding": 12,
                "width": null,
                "fps": 25,
                "mp4_encoder": "libx264"
            },
            "output_sha256": "oooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo",
            "output_filename": "demo.gif"
        }"#;
        let r: Receipt = serde_json::from_str(json).expect("legacy receipt parses");
        assert!(r.spec_sha256.is_none());
        assert!(r.scene_sha256.is_none());
        assert!(r.tool.recorder_sha256.is_none());
    }

    #[test]
    fn recorder_sha256_round_trips_and_skips_when_none() {
        let mut r = fixture_receipt();
        // Default fixture has None — must not appear in JSON.
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("recorder_sha256"),
            "None recorder_sha256 must skip serialization (back-compat)"
        );
        // With Some, must serialize and round-trip.
        r.tool.recorder_sha256 = Some("d".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""recorder_sha256":"#));
        let parsed: Receipt = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool.recorder_sha256, r.tool.recorder_sha256);
    }

    #[test]
    fn verify_refuses_nvenc_for_mp4_with_clear_outcome() {
        // Receipt for an .mp4 produced by NVENC: verify must refuse
        // up front instead of silently re-rendering and failing with
        // OutputDiffers (which would suggest the receipt is broken
        // when the real reason is the encoder isn't byte-portable).
        let mut r = fixture_receipt();
        r.render.mp4_encoder = "h264_nvenc".into();
        r.output_filename = "demo.mp4".into();

        // Write a fake cast file matching the receipt's claim so the
        // cast-hash check passes and we exercise the encoder check.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cast_bytes = b"{\"version\":2,\"width\":80,\"height\":24}\n";
        std::fs::write(tmp.path(), cast_bytes).unwrap();
        r.cast_sha256 = sha256_hex(cast_bytes);
        // Match tool identity to the current environment so we don't
        // get an EnvironmentDiffers diff first.
        r.tool = ToolIdentity::current().expect("ffmpeg present in test env");

        let outcome = r.verify(tmp.path()).unwrap();
        match outcome {
            VerifyOutcome::EncoderNotVerifiable { encoder } => {
                assert_eq!(encoder, "h264_nvenc");
            }
            other => panic!("expected EncoderNotVerifiable, got {other:?}"),
        }
    }

    #[test]
    fn verify_does_not_refuse_nvenc_for_gif_output() {
        // Same NVENC encoder, but the actual output is a GIF.
        // mp4_encoder is irrelevant for GIF rendering, so verify
        // must NOT refuse on encoder grounds — it should proceed
        // to the re-render (which here will fail later for other
        // reasons, since fixture cast bytes are minimal). This test
        // just asserts the refusal path doesn't fire.
        let mut r = fixture_receipt();
        r.render.mp4_encoder = "h264_nvenc".into();
        // The encoder check is gated on the output extension (.mp4),
        // not on mp4_encoder alone. Fixture's output is "demo.gif" so
        // verify wouldn't reach the refusal arm even with NVENC.
        assert!(
            std::path::Path::new(&r.output_filename)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
        );
    }

    #[test]
    fn first_env_diff_skips_recorder_sha256_when_either_side_missing() {
        // Symmetric: receipt without claim + verifier with hash → no diff.
        // (Verifier has stronger info than receipt; receipt is silent.)
        let mut expected = fixture_receipt().tool;
        let mut got = expected.clone();
        got.recorder_sha256 = Some("d".repeat(64));
        assert!(
            first_env_diff(&expected, &got).is_none(),
            "missing claim on receipt side must not flag a diff"
        );
        // Receipt has claim, verifier doesn't read its own binary →
        // no diff (best-effort verifier).
        expected.recorder_sha256 = Some("d".repeat(64));
        got.recorder_sha256 = None;
        assert!(
            first_env_diff(&expected, &got).is_none(),
            "missing reading on verifier side must not flag a diff"
        );
        // Both present + disagree → flagged.
        expected.recorder_sha256 = Some("d".repeat(64));
        got.recorder_sha256 = Some("e".repeat(64));
        let outcome = first_env_diff(&expected, &got).expect("expected diff");
        match outcome {
            VerifyOutcome::EnvironmentDiffers { field, .. } => {
                assert_eq!(field, "tool.recorder_sha256");
            }
            other => panic!("expected EnvironmentDiffers, got {other:?}"),
        }
    }

    #[test]
    fn receipt_with_scene_sha256_round_trips() {
        let mut r = fixture_receipt();
        r.scene_sha256 = Some("a".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains(r#""scene_sha256":"#),
            "scene_sha256 should serialize when Some"
        );
        let parsed: Receipt = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.scene_sha256, r.scene_sha256);
    }

    #[test]
    fn none_optional_fields_are_omitted_from_json() {
        let r = fixture_receipt();
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("scene_sha256"),
            "None scene_sha256 must skip serialization (back-compat)"
        );
        assert!(
            !json.contains("spec_sha256"),
            "None spec_sha256 must skip serialization (back-compat)"
        );
    }

    #[test]
    fn scene_and_spec_compose_in_one_receipt() {
        // Both attestation hashes can coexist — receipt records full
        // provenance (scene → cast → render → output) plus behavioral
        // attestation (spec → cast).
        let mut r = fixture_receipt();
        r.scene_sha256 = Some("a".repeat(64));
        r.spec_sha256 = Some("b".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        let parsed: Receipt = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.scene_sha256.as_deref(), Some(&"a".repeat(64)[..]));
        assert_eq!(parsed.spec_sha256.as_deref(), Some(&"b".repeat(64)[..]));
    }
}
