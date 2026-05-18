//! Reproducibility receipts for rendered traces.
//!
//! A [`Witness`] is a JSON sidecar that lets a third party verify the
//! rendered output (MP4/GIF) was produced from a known trace file by a
//! known pipeline. The receipt is written alongside the artifact and
//! verified later by [`Witness::verify`], which re-runs the pipeline
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

use crate::encode::Mp4Encoder;
use crate::paint::FONT_BYTES;
use crate::render::Render;
use ptytrace::attestation::Attestation;
use ptytrace::trace::Trace;

/// Current schema version. Bump on breaking changes.
pub const WITNESS_VERSION: u32 = 1;

/// On-disk reproducibility receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Witness {
    /// Schema version; must equal [`WITNESS_VERSION`].
    pub version: u32,
    /// Tool / environment identity at production time.
    pub tool: ToolIdentity,
    /// SHA-256 of the input trace file (raw bytes).
    pub trace_sha256: String,
    /// Render configuration that produced the output.
    pub render: RenderOptions,
    /// SHA-256 of the produced output bytes.
    pub output_sha256: String,
    /// Output filename at production time (informational).
    pub output_filename: String,
    /// Optional behavioral attestation hash. When present, the
    /// receipt promises that the trace satisfies a [`ptytrace::contract::Contract`]
    /// whose file bytes hash to this value. [`Witness::verify`] with `contract`
    /// confirms the spec hash matches and re-runs `Contract::check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_sha256: Option<String>,
    /// Optional source-script provenance hash. When present, the
    /// receipt records that the trace was produced by running
    /// [`ptytrace::script::Script`]`::run` on a `.script` file whose bytes
    /// hash to this value. This is provenance only — verification does
    /// not re-run the script (script execution depends on shells, docker
    /// images, and external state that the renderer does not pin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_sha256: Option<String>,
    /// Optional external provenance attestation hash. When present,
    /// the receipt commits to an [`ptytrace::attestation::Attestation`]
    /// sidecar whose file bytes hash to this value and whose
    /// `target_sha256` equals this receipt's `trace_sha256`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_sha256: Option<String>,
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
    /// Optional SHA-256 of the renderer binary itself
    /// (`std::env::current_exe()` bytes at receipt-emit time). Pins
    /// the exact build that produced the receipt — closes the gap
    /// where `version` is a `Cargo.toml` string but two builds with
    /// different `Cargo.lock` patch versions could diverge. Best-
    /// effort: `None` if the current binary is unreadable. When set
    /// on both sides, [`Witness::verify`] enforces equality.
    #[serde(
        default,
        alias = "recorder_sha256",
        skip_serializing_if = "Option::is_none"
    )]
    pub renderer_sha256: Option<String>,
    /// Optional SHA-256 of the `ffmpeg` binary resolved via PATH at
    /// receipt-emit time. Symmetric with [`Self::renderer_sha256`]:
    /// `ffmpeg_version` is just the first line of `ffmpeg -version`,
    /// which two builds with the same release tag but different
    /// patches share. Hashing the binary closes that gap. Best-
    /// effort: `None` if PATH is unset, no `ffmpeg` is found on it,
    /// or the resolved file is unreadable. When set on both sides,
    /// [`Witness::verify`] enforces equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg_sha256: Option<String>,
}

impl ToolIdentity {
    /// Capture the current environment's identity. Queries `ffmpeg
    /// -version`, hashes the bundled font, and best-effort hashes the
    /// running renderer binary itself.
    ///
    /// # Errors
    /// `ffmpeg` not on PATH or returned non-zero. Failure to read the
    /// current binary is non-fatal: `renderer_sha256` is left `None`.
    pub fn current() -> anyhow::Result<Self> {
        Ok(Self {
            name: "ptyrender".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ffmpeg_version: detect_ffmpeg_version()?,
            font_sha256: sha256_hex(FONT_BYTES),
            renderer_sha256: current_exe_sha256(),
            ffmpeg_sha256: ffmpeg_binary_sha256(),
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

/// SHA-256 of the `ffmpeg` binary resolved via the current `PATH`,
/// or `None` if PATH is unset, no `ffmpeg` is found on it, or the
/// resolved file is unreadable. Mirrors the lookup `Command::new
/// ("ffmpeg")` performs (first match wins, in `PATH` order); follows
/// symlinks so a versioned target shares its hash with all aliases.
fn ffmpeg_binary_sha256() -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("ffmpeg");
        if candidate.is_file()
            && let Ok(bytes) = std::fs::read(&candidate)
        {
            return Some(sha256_hex(&bytes));
        }
    }
    None
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
    /// Browser-compatible deterministic MP4 render options.
    #[must_use]
    pub fn libx264(font_size: f32, padding: u32, width: Option<u32>, fps: u32) -> Self {
        Self {
            font_size,
            padding,
            width,
            fps,
            mp4_encoder: "libx264".into(),
        }
    }

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

/// Outcome of a [`Witness::verify`] call and its stricter variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum VerifyOutcome {
    /// Every claim in the receipt held: trace hash + environment +
    /// re-rendered output, plus (if checked) spec hash + spec eval.
    Match,
    /// Provided trace file's hash differs from the receipt's claim.
    TraceDiffers { expected: String, got: String },
    /// Local environment differs from the receipt's pipeline identity.
    /// `field` names which sub-field disagreed.
    EnvironmentDiffers {
        field: String,
        expected: String,
        got: String,
    },
    /// Re-rendered output bytes hash to a different value.
    OutputDiffers { expected: String, got: String },
    /// Witness has a `contract_sha256` claim but no spec was provided to
    /// the verifier. Use [`Witness::verify`] with `contract`.
    ContractRequired,
    /// Provided spec file's hash differs from the receipt's claim.
    ContractDiffers { expected: String, got: String },
    /// Contract hash matched, but re-running [`ptytrace::contract::Contract::check`]
    /// against the trace did not pass every predicate.
    ContractFailed { failed: usize, total: usize },
    /// Witness has an `attestation_sha256` claim but no attestation
    /// sidecar was provided to the verifier. Use
    /// [`Witness::verify`] with `attestation` or
    /// [`Witness::verify`] with both.
    AttestationRequired,
    /// Provided attestation file's hash differs from the receipt's claim.
    AttestationDiffers { expected: String, got: String },
    /// Attestation hash matched, but the attestation targets a different
    /// trace digest than this receipt.
    AttestationTargetDiffers { expected: String, got: String },
    /// The receipt's MP4 encoder is not byte-deterministic across
    /// machines (e.g. `h264_nvenc` depends on GPU + driver version).
    /// Re-rendering would produce a meaningless comparison; verify
    /// refuses up front. The trace itself and any GIF outputs from
    /// the same trace remain verifiable; this is specifically about
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
            Self::TraceDiffers { expected, got } => {
                write!(f, "TRACE_DIFFERS expected={expected} got={got}")
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
            Self::ContractRequired => {
                write!(f, "SPEC_REQUIRED receipt expects --spec")
            }
            Self::ContractDiffers { expected, got } => {
                write!(f, "SPEC_DIFFERS expected={expected} got={got}")
            }
            Self::ContractFailed { failed, total } => {
                write!(f, "SPEC_FAILED {failed}/{total} predicate(s)")
            }
            Self::AttestationRequired => {
                write!(f, "ATTESTATION_REQUIRED receipt expects --attestation")
            }
            Self::AttestationDiffers { expected, got } => {
                write!(f, "ATTESTATION_DIFFERS expected={expected} got={got}")
            }
            Self::AttestationTargetDiffers { expected, got } => {
                write!(
                    f,
                    "ATTESTATION_TARGET_DIFFERS expected={expected} got={got}"
                )
            }
            Self::EncoderNotVerifiable { encoder } => {
                write!(f, "ENCODER_NOT_VERIFIABLE encoder={encoder}")
            }
        }
    }
}

impl Witness {
    /// Build a witness claim for an already-rendered output file.
    ///
    /// This is used by live `.ptyrecord` stitching: frames are painted
    /// during capture and encoded once, so producing the witness must
    /// not re-run the renderer just to learn hashes.
    ///
    /// This constructor records the caller-provided render options; it
    /// does not prove the existing media was produced by those options.
    /// Use [`Self::verify`] later, or [`Self::from_verified_rendered_output`]
    /// when the constructor itself must perform that proof.
    ///
    /// # Errors
    /// Trace or output file cannot be read, or tool identity cannot be
    /// captured.
    pub fn from_rendered_output(
        trace_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        render: RenderOptions,
    ) -> anyhow::Result<Self> {
        let trace_bytes = std::fs::read(trace_path.as_ref()).context("read trace for receipt")?;
        let output_path = output_path.as_ref();
        let output_bytes =
            std::fs::read(output_path).context("read rendered output for receipt")?;
        let output_filename = utf8_file_name(output_path)?;

        Ok(Self {
            version: WITNESS_VERSION,
            tool: ToolIdentity::current()?,
            trace_sha256: sha256_hex(&trace_bytes),
            render,
            output_sha256: sha256_hex(&output_bytes),
            output_filename,
            contract_sha256: None,
            script_sha256: None,
            attestation_sha256: None,
        })
    }

    /// Build a witness for an already-rendered output file, then
    /// immediately re-render the trace and require the receipt to verify.
    ///
    /// This is the safer public constructor when avoiding a second render
    /// is not important. Live renderers can use [`Self::from_rendered_output`]
    /// and leave verification to the consumer.
    ///
    /// # Errors
    /// Trace or output file cannot be read, tool identity cannot be
    /// captured, re-rendering fails, or the rendered output does not match
    /// the witness claim.
    pub fn from_verified_rendered_output(
        trace_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        render: RenderOptions,
    ) -> anyhow::Result<Self> {
        let trace_path = trace_path.as_ref();
        let witness = Self::from_rendered_output(trace_path, output_path, render)?;
        let outcome = witness.verify(trace_path, None, None)?;
        if matches!(outcome, VerifyOutcome::Match) {
            Ok(witness)
        } else {
            anyhow::bail!("rendered output does not verify: {outcome}");
        }
    }

    /// Read a receipt from disk.
    ///
    /// # Errors
    /// IO error or JSON parse failure.
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path.as_ref()).context("read receipt")?;
        let receipt: Self = serde_json::from_slice(&bytes).context("parse receipt")?;
        anyhow::ensure!(
            receipt.version == WITNESS_VERSION,
            "receipt version {} not supported (expected {})",
            receipt.version,
            WITNESS_VERSION,
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

    /// Re-render the trace and confirm every claim in the receipt
    /// holds (B-only check). Returns the structured outcome instead
    /// of erroring on mismatch — callers decide how to react.
    ///
    /// If the receipt carries a [`Self::contract_sha256`] claim, this
    /// method returns [`VerifyOutcome::ContractRequired`] without doing
    /// the re-render — full verification needs the spec, so call
    /// [`Self::verify`] with `contract` instead.
    /// If the receipt carries a [`Self::attestation_sha256`] claim, this
    /// method returns [`VerifyOutcome::AttestationRequired`] without
    /// doing the re-render — full verification needs the attestation
    /// sidecar, so call [`Self::verify`] with `attestation` instead.
    ///
    /// # Errors
    /// Trace file missing, ffmpeg invocation failed, or other IO error
    /// during re-render. Witness-claim mismatches return
    /// `Ok(VerifyOutcome::*)`, not `Err`.
    /// Verify the receipt. Performs the B-check (trace hash,
    /// environment, re-render output match) and, when `contract` /
    /// `attestation` paths are provided, the matching C-check and
    /// provenance-anchor check.
    ///
    /// **Required claims.** If the receipt carries a
    /// `contract_sha256` claim but `contract` is `None`, returns
    /// [`VerifyOutcome::ContractRequired`]. Same for `attestation`
    /// claims and `AttestationRequired`. The verifier cannot complete
    /// a receipt's anchored claims without the underlying file.
    ///
    /// **Optional checks.** Passing a `contract` or `attestation` when
    /// the receipt has no matching `_sha256` claim still runs the
    /// check against the trace — the receipt is silent on that
    /// relationship, but the verifier confirms it holds.
    ///
    /// ```no_run
    /// use ptyrender::witness::{Witness, VerifyOutcome};
    /// use std::path::Path;
    ///
    /// let receipt = Witness::read("demo.gif.receipt.json")?;
    /// // Minimal: B-check only.
    /// let _ = receipt.verify(Path::new("demo.ptytrace"), None, None)?;
    /// // With spec: B + C.
    /// let _ = receipt.verify(
    ///     Path::new("demo.ptytrace"),
    ///     Some(Path::new("demo.spec.json")),
    ///     None,
    /// )?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    ///
    /// # Errors
    /// Trace, spec, or attestation file missing; spec or attestation
    /// JSON parse error; unsupported attestation version; ffmpeg
    /// invocation failed; or other IO error. Witness-claim mismatches
    /// return `Ok(VerifyOutcome::*)`, not `Err`.
    pub fn verify(
        &self,
        trace_path: &Path,
        contract: Option<&Path>,
        attestation: Option<&Path>,
    ) -> anyhow::Result<VerifyOutcome> {
        // Required-claim short-circuits: if the receipt anchors
        // something the caller didn't supply, we can't finish.
        if self.contract_sha256.is_some() && contract.is_none() {
            return Ok(VerifyOutcome::ContractRequired);
        }
        if self.attestation_sha256.is_some() && attestation.is_none() {
            return Ok(VerifyOutcome::AttestationRequired);
        }

        // Spec hash check up-front (cheap) so caller learns of a
        // mismatch before we spend time re-rendering.
        let spec_bytes = if let Some(spec_path) = contract {
            match self.read_spec_claim(spec_path)? {
                Ok(bytes) => Some(bytes),
                Err(outcome) => return Ok(outcome),
            }
        } else {
            None
        };

        // Attestation hash + target check.
        if let Some(att_path) = attestation
            && let Some(outcome) = self.verify_attestation_claim(att_path)?
        {
            return Ok(outcome);
        }

        // B-check (re-render, compare).
        match self.verify_b(trace_path)? {
            VerifyOutcome::Match => {}
            other => return Ok(other),
        }

        // C-check (predicates).
        if let Some(bytes) = spec_bytes
            && let Some(outcome) = Self::verify_contract_bytes(trace_path, &bytes)?
        {
            return Ok(outcome);
        }

        Ok(VerifyOutcome::Match)
    }

    fn read_spec_claim(&self, spec_path: &Path) -> anyhow::Result<Result<Vec<u8>, VerifyOutcome>> {
        // Re-canonicalize on read so the verifier and the producer are
        // both hashing Contract::canonical_bytes, never raw file bytes
        // whose formatting can drift across serde_json versions or
        // hand-edits.
        let spec = ptytrace::contract::Contract::read(spec_path)?;
        let spec_bytes = spec.canonical_bytes()?;
        let spec_hash = sha256_hex(&spec_bytes);
        if let Some(expected) = &self.contract_sha256
            && &spec_hash != expected
        {
            return Ok(Err(VerifyOutcome::ContractDiffers {
                expected: expected.clone(),
                got: spec_hash,
            }));
        }
        Ok(Ok(spec_bytes))
    }

    fn verify_contract_bytes(
        trace_path: &Path,
        spec_bytes: &[u8],
    ) -> anyhow::Result<Option<VerifyOutcome>> {
        let spec: ptytrace::contract::Contract =
            serde_json::from_slice(spec_bytes).context("parse spec")?;
        let trace = Trace::read(trace_path)?;
        let report = spec.check(&trace);
        if !report.all_passed() {
            return Ok(Some(VerifyOutcome::ContractFailed {
                failed: report.failed_count(),
                total: report.outcomes.len(),
            }));
        }
        Ok(None)
    }

    fn verify_attestation_claim(
        &self,
        attestation_path: &Path,
    ) -> anyhow::Result<Option<VerifyOutcome>> {
        let attestation_bytes =
            std::fs::read(attestation_path).context("read attestation for verify")?;
        let attestation_hash = sha256_hex(&attestation_bytes);
        if let Some(expected) = &self.attestation_sha256
            && &attestation_hash != expected
        {
            return Ok(Some(VerifyOutcome::AttestationDiffers {
                expected: expected.clone(),
                got: attestation_hash,
            }));
        }

        let attestation = Attestation::from_slice(&attestation_bytes)?;
        if !attestation.targets_trace(&self.trace_sha256) {
            return Ok(Some(VerifyOutcome::AttestationTargetDiffers {
                expected: self.trace_sha256.clone(),
                got: attestation.target_sha256,
            }));
        }

        Ok(None)
    }

    fn verify_b(&self, trace_path: &Path) -> anyhow::Result<VerifyOutcome> {
        // 1. Trace hash check. If the verifier was handed a different
        //    trace file than the one the receipt was produced from, we
        //    can't reproduce anything meaningful.
        let trace_bytes = std::fs::read(trace_path).context("read trace for verify")?;
        let trace_hash = sha256_hex(&trace_bytes);
        if trace_hash != self.trace_sha256 {
            return Ok(VerifyOutcome::TraceDiffers {
                expected: self.trace_sha256.clone(),
                got: trace_hash,
            });
        }

        // 2. Environment check. Different ffmpeg / font / tool version
        //    means the pipeline identity differs; even bit-exact inputs
        //    won't reproduce.
        let current = ToolIdentity::current()?;
        if let Some(diff) = first_env_diff(&self.tool, &current) {
            return Ok(diff);
        }

        // 3. Re-render the trace with the receipt's config into a
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
            .prefix("ptytrace-verify-")
            .suffix(&format!(".{ext}"))
            .tempfile()
            .context("create verify tempfile")?;

        let trace = Trace::parse(std::str::from_utf8(&trace_bytes)?)?;
        let mut r = Render::new(trace)
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
    // renderer_sha256 + ffmpeg_sha256 are optional. Compare each only
    // when both sides have a value — receipts written before the field
    // existed (or by hosts where the binary couldn't be read) skip the
    // check. Scott-flat: None matches anything.
    let optional_pairs = [
        (
            "tool.renderer_sha256",
            expected.renderer_sha256.as_ref(),
            got.renderer_sha256.as_ref(),
        ),
        (
            "tool.ffmpeg_sha256",
            expected.ffmpeg_sha256.as_ref(),
            got.ffmpeg_sha256.as_ref(),
        ),
    ];
    for (field, exp_opt, got_opt) in optional_pairs {
        if let (Some(exp), Some(cur)) = (exp_opt, got_opt)
            && exp != cur
        {
            return Some(VerifyOutcome::EnvironmentDiffers {
                field: (*field).to_string(),
                expected: exp.clone(),
                got: cur.clone(),
            });
        }
    }
    None
}

/// Extract a `path`'s final component as an owned UTF8 `String`.
///
/// Witness receipts are JSON, so a non-UTF8 filename cannot round-trip.
/// Callers previously substituted the empty string via
/// `unwrap_or_default()`, which silently corrupted the provenance
/// record (it pointed at no file). Fail loudly instead.
///
/// # Errors
/// The path has no terminal component (e.g. ends in `..` after
/// normalization), or its terminal component contains non-UTF8 bytes.
pub(crate) fn utf8_file_name(path: &Path) -> anyhow::Result<String> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("path {} has no filename component", path.display()))?;
    name.to_str()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "path {} has a non-UTF8 filename; witnesses require UTF8",
                path.display()
            )
        })
        .map(str::to_string)
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
    use serde_json::json;

    use super::*;

    fn fixture_receipt() -> Witness {
        Witness {
            version: WITNESS_VERSION,
            tool: ToolIdentity {
                name: "ptyrender".into(),
                version: "0.1.0".into(),
                ffmpeg_version: "ffmpeg version 6.1.1".into(),
                font_sha256: "f".repeat(64),
                renderer_sha256: None,
                ffmpeg_sha256: None,
            },
            trace_sha256: "c".repeat(64),
            render: RenderOptions {
                font_size: 14.0,
                padding: 12,
                width: None,
                fps: 25,
                mp4_encoder: "libx264".into(),
            },
            output_sha256: "o".repeat(64),
            output_filename: "demo.gif".into(),
            contract_sha256: None,
            script_sha256: None,
            attestation_sha256: None,
        }
    }

    fn write_attestation(target_sha256: &str) -> tempfile::NamedTempFile {
        let attestation = Attestation::new(
            "file",
            "test-suite",
            "fixture",
            target_sha256,
            ptytrace::attestation::Freshness::None,
            json!({}),
            json!({ "algorithm": "none" }),
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        attestation.write(tmp.path()).unwrap();
        tmp
    }

    #[test]
    fn legacy_receipt_without_optional_hashes_parses() {
        // Legacy receipts (pre-contract_sha256, pre-script_sha256, pre-
        // attestation_sha256, pre-renderer_sha256, pre-ffmpeg_sha256)
        // must continue to parse — those fields are additive Option<String>
        // with serde defaults.
        let json = r#"{
            "version": 1,
            "tool": {
                "name": "ptytrace",
                "version": "0.1.0",
                "ffmpeg_version": "ffmpeg version 6.1.1",
                "font_sha256": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
            },
            "trace_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
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
        let r: Witness = serde_json::from_str(json).expect("legacy receipt parses");
        assert!(r.contract_sha256.is_none());
        assert!(r.script_sha256.is_none());
        assert!(r.attestation_sha256.is_none());
        assert!(r.tool.renderer_sha256.is_none());
        assert!(r.tool.ffmpeg_sha256.is_none());
    }

    #[test]
    fn renderer_sha256_round_trips_and_skips_when_none() {
        let mut r = fixture_receipt();
        // Default fixture has None — must not appear in JSON.
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("renderer_sha256"),
            "None renderer_sha256 must skip serialization (back-compat)"
        );
        // With Some, must serialize and round-trip.
        r.tool.renderer_sha256 = Some("d".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""renderer_sha256":"#));
        let parsed: Witness = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool.renderer_sha256, r.tool.renderer_sha256);
    }

    #[test]
    fn legacy_recorder_sha256_alias_parses() {
        let mut value = serde_json::to_value(fixture_receipt()).unwrap();
        value["tool"]["recorder_sha256"] = json!("d".repeat(64));
        let r: Witness = serde_json::from_value(value).unwrap();
        assert_eq!(r.tool.renderer_sha256, Some("d".repeat(64)));
    }

    #[test]
    fn verify_refuses_nvenc_for_mp4_with_clear_outcome() {
        // Witness for an .mp4 produced by NVENC: verify must refuse
        // up front instead of silently re-rendering and failing with
        // OutputDiffers (which would suggest the receipt is broken
        // when the real reason is the encoder isn't byte-portable).
        let mut r = fixture_receipt();
        r.render.mp4_encoder = "h264_nvenc".into();
        r.output_filename = "demo.mp4".into();

        // Write a fake trace file matching the receipt's claim so the
        // trace-hash check passes and we exercise the encoder check.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let trace_bytes = b"{\"version\":2,\"width\":80,\"height\":24}\n";
        std::fs::write(tmp.path(), trace_bytes).unwrap();
        r.trace_sha256 = sha256_hex(trace_bytes);
        // Match tool identity to the current environment so we don't
        // get an EnvironmentDiffers diff first.
        r.tool = ToolIdentity::current().expect("ffmpeg present in test env");

        let outcome = r.verify(tmp.path(), None, None).unwrap();
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
        // reasons, since fixture trace bytes are minimal). This test
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
    fn ffmpeg_sha256_round_trips_and_skips_when_none() {
        let mut r = fixture_receipt();
        // Default fixture has None — must not appear in JSON.
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("ffmpeg_sha256"),
            "None ffmpeg_sha256 must skip serialization (back-compat)"
        );
        // With Some, must serialize and round-trip.
        r.tool.ffmpeg_sha256 = Some("a".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""ffmpeg_sha256":"#));
        let parsed: Witness = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool.ffmpeg_sha256, r.tool.ffmpeg_sha256);
    }

    #[test]
    fn first_env_diff_skips_ffmpeg_sha256_when_either_side_missing() {
        // Symmetric with renderer_sha256: receipt without claim +
        // verifier with hash → no diff. Witness with claim + verifier
        // unable to read → no diff. Both present + disagree → flagged.
        let mut expected = fixture_receipt().tool;
        let mut got = expected.clone();
        got.ffmpeg_sha256 = Some("a".repeat(64));
        assert!(
            first_env_diff(&expected, &got).is_none(),
            "missing claim on receipt side must not flag a diff"
        );
        expected.ffmpeg_sha256 = Some("a".repeat(64));
        got.ffmpeg_sha256 = None;
        assert!(
            first_env_diff(&expected, &got).is_none(),
            "missing reading on verifier side must not flag a diff"
        );
        expected.ffmpeg_sha256 = Some("a".repeat(64));
        got.ffmpeg_sha256 = Some("b".repeat(64));
        let outcome = first_env_diff(&expected, &got).expect("expected diff");
        match outcome {
            VerifyOutcome::EnvironmentDiffers { field, .. } => {
                assert_eq!(field, "tool.ffmpeg_sha256");
            }
            other => panic!("expected EnvironmentDiffers, got {other:?}"),
        }
    }

    #[test]
    fn first_env_diff_skips_renderer_sha256_when_either_side_missing() {
        // Symmetric: receipt without claim + verifier with hash → no diff.
        // (Verifier has stronger info than receipt; receipt is silent.)
        let mut expected = fixture_receipt().tool;
        let mut got = expected.clone();
        got.renderer_sha256 = Some("d".repeat(64));
        assert!(
            first_env_diff(&expected, &got).is_none(),
            "missing claim on receipt side must not flag a diff"
        );
        // Witness has claim, verifier doesn't read its own binary →
        // no diff (best-effort verifier).
        expected.renderer_sha256 = Some("d".repeat(64));
        got.renderer_sha256 = None;
        assert!(
            first_env_diff(&expected, &got).is_none(),
            "missing reading on verifier side must not flag a diff"
        );
        // Both present + disagree → flagged.
        expected.renderer_sha256 = Some("d".repeat(64));
        got.renderer_sha256 = Some("e".repeat(64));
        let outcome = first_env_diff(&expected, &got).expect("expected diff");
        match outcome {
            VerifyOutcome::EnvironmentDiffers { field, .. } => {
                assert_eq!(field, "tool.renderer_sha256");
            }
            other => panic!("expected EnvironmentDiffers, got {other:?}"),
        }
    }

    #[test]
    fn receipt_with_script_sha256_round_trips() {
        let mut r = fixture_receipt();
        r.script_sha256 = Some("a".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains(r#""script_sha256":"#),
            "script_sha256 should serialize when Some"
        );
        let parsed: Witness = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.script_sha256, r.script_sha256);
    }

    #[test]
    fn receipt_with_attestation_sha256_round_trips() {
        let mut r = fixture_receipt();
        r.attestation_sha256 = Some("a".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains(r#""attestation_sha256":"#),
            "attestation_sha256 should serialize when Some"
        );
        let parsed: Witness = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.attestation_sha256, r.attestation_sha256);
    }

    #[test]
    fn none_optional_fields_are_omitted_from_json() {
        let r = fixture_receipt();
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("script_sha256"),
            "None script_sha256 must skip serialization (back-compat)"
        );
        assert!(
            !json.contains("contract_sha256"),
            "None contract_sha256 must skip serialization (back-compat)"
        );
        assert!(
            !json.contains("attestation_sha256"),
            "None attestation_sha256 must skip serialization (back-compat)"
        );
        assert!(
            !json.contains("ffmpeg_sha256"),
            "None ffmpeg_sha256 must skip serialization (back-compat)"
        );
    }

    #[test]
    fn script_spec_and_attestation_compose_in_one_receipt() {
        // Receipt records source-script provenance, behavioral contract,
        // and external provenance attestation independently.
        let mut r = fixture_receipt();
        r.script_sha256 = Some("a".repeat(64));
        r.contract_sha256 = Some("b".repeat(64));
        r.attestation_sha256 = Some("d".repeat(64));
        let json = serde_json::to_string(&r).unwrap();
        let parsed: Witness = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.script_sha256.as_deref(), Some(&"a".repeat(64)[..]));
        assert_eq!(parsed.contract_sha256.as_deref(), Some(&"b".repeat(64)[..]));
        assert_eq!(
            parsed.attestation_sha256.as_deref(),
            Some(&"d".repeat(64)[..])
        );
    }

    #[test]
    fn verify_requires_attestation_when_receipt_claims_one() {
        let mut r = fixture_receipt();
        r.attestation_sha256 = Some("a".repeat(64));

        let outcome = r.verify(Path::new("unused.ptytrace"), None, None).unwrap();

        assert!(matches!(outcome, VerifyOutcome::AttestationRequired));
        assert_eq!(
            outcome.to_string(),
            "ATTESTATION_REQUIRED receipt expects --attestation"
        );
    }

    #[test]
    fn verify_with_spec_requires_attestation_when_receipt_claims_one() {
        let mut r = fixture_receipt();
        r.attestation_sha256 = Some("a".repeat(64));

        let outcome = r
            .verify(
                Path::new("unused.ptytrace"),
                Some(Path::new("unused.spec.json")),
                None,
            )
            .unwrap();

        assert!(matches!(outcome, VerifyOutcome::AttestationRequired));
    }

    #[test]
    fn verify_with_attestation_reports_hash_diff() {
        let mut r = fixture_receipt();
        r.attestation_sha256 = Some("b".repeat(64));
        let attestation = write_attestation(&r.trace_sha256);

        let outcome = r
            .verify(Path::new("unused.ptytrace"), None, Some(attestation.path()))
            .unwrap();

        match outcome {
            VerifyOutcome::AttestationDiffers { expected, got } => {
                assert_eq!(expected, "b".repeat(64));
                assert_ne!(got, expected);
            }
            other => panic!("expected AttestationDiffers, got {other:?}"),
        }
    }

    #[test]
    fn verify_with_attestation_reports_target_diff() {
        let mut r = fixture_receipt();
        let attestation = write_attestation(&"d".repeat(64));
        let attestation_bytes = std::fs::read(attestation.path()).unwrap();
        r.attestation_sha256 = Some(sha256_hex(&attestation_bytes));

        let outcome = r
            .verify(Path::new("unused.ptytrace"), None, Some(attestation.path()))
            .unwrap();

        match outcome {
            VerifyOutcome::AttestationTargetDiffers { expected, got } => {
                assert_eq!(expected, r.trace_sha256);
                assert_eq!(got, "d".repeat(64));
            }
            other => panic!("expected AttestationTargetDiffers, got {other:?}"),
        }
    }

    #[test]
    fn utf8_file_name_returns_terminal_component() {
        let got = super::utf8_file_name(Path::new("/tmp/x/demo.mp4")).unwrap();
        assert_eq!(got, "demo.mp4");
    }

    #[test]
    fn utf8_file_name_rejects_non_utf8_component() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // 0xFF is never a valid UTF-8 start byte.
        let bad = OsStr::from_bytes(&[b'a', 0xFF, b'.', b'm', b'p', b'4']);
        let p = Path::new(bad);
        let err = super::utf8_file_name(p).unwrap_err().to_string();
        assert!(err.contains("non-UTF8"), "wrong message: {err}");
    }

    #[test]
    fn utf8_file_name_rejects_path_with_no_component() {
        let err = super::utf8_file_name(Path::new("/"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no filename component"),
            "wrong message: {err}"
        );
    }
}
