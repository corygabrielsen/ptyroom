//! Filesystem helpers for the [`crate::attestation`] types.
//!
//! Used cross-crate by the `ptytrace` and `ptyrender` binaries to
//! build, write, and verify attestation sidecars. Kept separate from
//! [`crate::attestation`] so the core types stay free of IO
//! concerns — `attestation` is data + signing; this module is the
//! file-based interface.

use std::path::Path;

use anyhow::Context;
use serde_json::json;

use crate::attestation::{
    Attestation, AttestationProvider, FileAttestationProvider, Freshness, sha256_hex,
};

const DEFAULT_FILE_ISSUER: &str = "ptytrace file provider";

/// Read the trace at `path` and return its `(sha256_hex, size_bytes)`.
///
/// # Errors
/// IO error reading the trace file.
pub fn trace_sha256(path: &Path) -> anyhow::Result<(String, usize)> {
    let bytes = std::fs::read(path).with_context(|| format!("read trace {}", path.display()))?;
    Ok((sha256_hex(&bytes), bytes.len()))
}

/// Default `subject` field for attestations targeting `path` — the
/// path's file name, or `"trace"` if extraction fails.
#[must_use]
pub fn default_file_subject(path: &Path) -> String {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("trace")
        .to_string()
}

/// Build an unsigned local-file attestation for the trace at
/// `trace_path`. `issuer`/`subject`/`nonce` default to internal
/// values when `None`.
///
/// # Errors
/// Attestation construction failure from
/// [`crate::attestation::AttestationProvider::attest`].
pub fn file_attestation(
    trace_path: &Path,
    trace_sha256: &str,
    trace_size_bytes: usize,
    issuer: Option<&str>,
    subject: Option<&str>,
    nonce: Option<&str>,
) -> anyhow::Result<Attestation> {
    let subject = subject.map_or_else(|| default_file_subject(trace_path), str::to_string);
    let freshness = nonce.map_or(Freshness::None, |value| Freshness::Nonce {
        nonce: value.to_string(),
    });
    let provider = FileAttestationProvider::new(
        issuer.unwrap_or(DEFAULT_FILE_ISSUER),
        subject,
        freshness,
        json!({
            "trace_filename": default_file_subject(trace_path),
            "trace_size_bytes": trace_size_bytes,
            "provider_note": "unsigned local file anchor"
        }),
    );
    provider.attest(trace_sha256)
}

/// Serialize `attestation` to JSON, write it to `path`, return the
/// SHA-256 hex of the written bytes (suitable for embedding as a
/// receipt's `attestation_sha256` claim).
///
/// # Errors
/// JSON serialization failure or IO error writing the file.
pub fn write_attestation(path: &Path, attestation: &Attestation) -> anyhow::Result<String> {
    let bytes = attestation.to_json_bytes()?;
    let sha256 = sha256_hex(&bytes);
    std::fs::write(path, bytes).with_context(|| format!("write attestation {}", path.display()))?;
    Ok(sha256)
}

/// Read the attestation at `attestation_path`, return its SHA-256 hex
/// after confirming it targets `trace_sha256`.
///
/// # Errors
/// IO error, JSON parse error, unsupported attestation version, or
/// target mismatch (attestation claims a different trace hash).
pub fn attestation_sha256_for_trace(
    attestation_path: &Path,
    trace_sha256: &str,
) -> anyhow::Result<String> {
    let bytes = std::fs::read(attestation_path)
        .with_context(|| format!("read attestation {}", attestation_path.display()))?;
    let sha256 = sha256_hex(&bytes);
    let attestation = Attestation::from_slice(&bytes)?;
    anyhow::ensure!(
        attestation.targets_trace(trace_sha256),
        "attestation {} targets {}, but trace hash is {}",
        attestation_path.display(),
        attestation.target_sha256,
        trace_sha256,
    );
    Ok(sha256)
}
