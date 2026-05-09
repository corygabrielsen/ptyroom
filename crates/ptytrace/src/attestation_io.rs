//! Shared CLI helpers for attestation sidecars.

use std::path::Path;

use anyhow::Context;
use serde_json::json;

use crate::attestation::{
    Attestation, AttestationProvider, FileAttestationProvider, Freshness, sha256_hex,
};

const DEFAULT_FILE_ISSUER: &str = "ptytrace file provider";

pub fn trace_sha256(path: &Path) -> anyhow::Result<(String, usize)> {
    let bytes = std::fs::read(path).with_context(|| format!("read trace {}", path.display()))?;
    Ok((sha256_hex(&bytes), bytes.len()))
}

pub fn default_file_subject(path: &Path) -> String {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("trace")
        .to_string()
}

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

pub fn write_attestation(path: &Path, attestation: &Attestation) -> anyhow::Result<String> {
    let bytes = attestation.to_json_bytes()?;
    let sha256 = sha256_hex(&bytes);
    std::fs::write(path, bytes).with_context(|| format!("write attestation {}", path.display()))?;
    Ok(sha256)
}

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
