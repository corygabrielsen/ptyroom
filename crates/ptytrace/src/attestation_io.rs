//! Filesystem helpers for the [`crate::attestation`] types.
//!
//! Used cross-crate by the `ptytrace` and `ptyrender` binaries to
//! build, write, and verify attestation sidecars. Kept separate from
//! [`crate::attestation`] so the core types stay free of IO
//! concerns — `attestation` is data + signing; this module is the
//! file-based interface.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::json;

use crate::attestation::{
    Attestation, AttestationProvider, FileAttestationProvider, Freshness, sha256_hex,
};

const DEFAULT_FILE_ISSUER: &str = "ptytrace file provider";

/// Compute a sibling `<path>.tmp` path for the atomic-rename stage in
/// [`write_attestation`]. The temp file lives next to the target so
/// `std::fs::rename` stays within a single filesystem (cross-device
/// rename is not atomic — and on POSIX, often fails outright with
/// `EXDEV`).
fn atomic_tmp_path(path: &Path) -> PathBuf {
    let mut tmp_name = path.file_name().map_or_else(OsString::new, OsString::from);
    tmp_name.push(".tmp");
    path.with_file_name(tmp_name)
}

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

/// Serialize `attestation` to JSON, atomically install it at `path`,
/// return the SHA-256 hex of the installed bytes (suitable for
/// embedding as a receipt's `attestation_sha256` claim).
///
/// # Atomicity
///
/// The bytes are first written to a sibling `<path>.tmp`, then
/// `rename`d over `path`. On POSIX `rename(2)` within a filesystem is
/// atomic, so the file at `path` is either the previous contents (if
/// any) or the full new contents — never a torn write. The returned
/// hash is the hash of the installed bytes: it is only returned
/// AFTER the rename succeeds, so a hash mismatch between the receipt
/// and the on-disk file is impossible (modulo external tampering
/// after the function returns).
///
/// If the temp-write fails (disk full, permission denied, interrupted
/// signal, etc.) the function returns an error and `path` is left
/// untouched. A best-effort cleanup removes the temp file on
/// rename failure; if the cleanup itself fails the stale temp is
/// left for an operator to inspect.
///
/// # Errors
/// JSON serialization failure or IO error writing or renaming the
/// file.
pub fn write_attestation(path: &Path, attestation: &Attestation) -> anyhow::Result<String> {
    let bytes = attestation.to_json_bytes()?;
    let tmp_path = atomic_tmp_path(path);
    std::fs::write(&tmp_path, &bytes)
        .with_context(|| format!("write attestation temp {}", tmp_path.display()))?;
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        // Rename failed — the target is untouched. Clean up the temp
        // before propagating the error so we do not leave stale
        // siblings on every retry.
        let _ = std::fs::remove_file(&tmp_path);
        return Err(anyhow::Error::new(err).context(format!(
            "rename attestation {} -> {}",
            tmp_path.display(),
            path.display(),
        )));
    }
    // Hash is computed from the in-memory buffer only AFTER rename
    // success. The bytes on disk at `path` are bit-identical to
    // `bytes`, so this is the hash of the on-disk file.
    Ok(sha256_hex(&bytes))
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn sample_attestation() -> Attestation {
        let trace = b"hello world";
        let trace_hash = sha256_hex(trace);
        file_attestation(
            Path::new("sample.ptytrace"),
            &trace_hash,
            trace.len(),
            Some("test issuer"),
            Some("sample.ptytrace"),
            None,
        )
        .unwrap()
    }

    #[test]
    fn atomic_tmp_path_is_a_sibling() {
        let target = PathBuf::from("/foo/bar/baz.json");
        let tmp = atomic_tmp_path(&target);
        assert_eq!(tmp.parent(), target.parent());
        assert_eq!(tmp.file_name().unwrap(), "baz.json.tmp");
    }

    #[test]
    fn write_attestation_returns_hash_matching_disk_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attest.json");
        let attestation = sample_attestation();

        let returned_sha = write_attestation(&path, &attestation).unwrap();

        let disk_bytes = std::fs::read(&path).unwrap();
        let disk_sha = sha256_hex(&disk_bytes);
        assert_eq!(
            returned_sha, disk_sha,
            "the returned hash MUST match the bytes on disk — otherwise \
             a receipt embedding the returned hash would not verify",
        );
    }

    #[test]
    fn write_attestation_leaves_no_temp_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attest.json");
        let attestation = sample_attestation();

        write_attestation(&path, &attestation).unwrap();

        let tmp = atomic_tmp_path(&path);
        assert!(
            !tmp.exists(),
            "temp file must not survive a successful rename"
        );
        assert!(
            path.exists(),
            "target file must exist after successful write"
        );
    }

    #[test]
    fn write_attestation_preserves_previous_contents_when_rename_fails() {
        // Rename will fail when the target path is a non-empty
        // directory (POSIX `rename` cannot replace a non-empty
        // directory with a regular file). We use that to simulate a
        // rename-stage failure WITHOUT mocking the filesystem.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("attest.json");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("sentinel"), b"untouched").unwrap();

        let attestation = sample_attestation();
        let result = write_attestation(&target, &attestation);

        assert!(
            result.is_err(),
            "rename onto non-empty directory must surface an error",
        );
        // The sentinel survives — the rename never replaced anything,
        // so the pre-existing state is intact.
        let sentinel = std::fs::read(target.join("sentinel")).unwrap();
        assert_eq!(sentinel, b"untouched");
        // Temp file cleaned up on failure.
        let tmp = atomic_tmp_path(&target);
        assert!(
            !tmp.exists(),
            "failed rename must clean up its `<target>.tmp` sibling",
        );
    }
}
