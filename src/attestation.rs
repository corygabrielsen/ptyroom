//! Provenance anchors for trace digests.
//!
//! A reproducibility witness proves media was rendered from a trace. An
//! attestation is the separate object that lets a provider claim an
//! identity, machine, service, or session bound itself to that trace.
//! The load-bearing invariant is [`Attestation::target_sha256`]: every
//! provider-specific proof must target the trace hash, not just describe
//! nearby metadata.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Current on-disk attestation schema version.
pub const ATTESTATION_VERSION: u32 = 1;

/// Built-in unsigned provider kind for local fixture/prototype anchors.
pub const FILE_ATTESTATION_KIND: &str = "file";

/// Verifiable provider claim over a trace digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Attestation {
    /// Schema version; must equal [`ATTESTATION_VERSION`].
    pub version: u32,
    /// Provider family, such as `ssh`, `kms`, `tpm`, `oidc`, or `file`.
    pub kind: String,
    /// Entity that issued or roots the claim.
    pub issuer: String,
    /// Entity the claim is about.
    pub subject: String,
    /// Provider-specific context. Examples: host fingerprint, role ARN,
    /// workflow run id, TPM PCR set, log index.
    pub context: serde_json::Value,
    /// SHA-256 of the trace bytes this attestation targets.
    pub target_sha256: String,
    /// Replay/freshness material, if the provider has any.
    pub freshness: Freshness,
    /// Provider-specific proof material. Examples: signature, TPM quote,
    /// OIDC token hash, transparency log inclusion proof.
    pub proof: serde_json::Value,
}

/// Replay/freshness material embedded in an [`Attestation`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Freshness {
    /// No freshness proof. Useful for fixtures and providers that only
    /// establish attribution.
    None,
    /// Provider signed or otherwise incorporated a nonce.
    Nonce { nonce: String },
    /// Provider supplied a timestamp in milliseconds since Unix epoch.
    Timestamp { unix_ms: u64 },
    /// Provider supplied both nonce and timestamp.
    NonceAndTimestamp { nonce: String, unix_ms: u64 },
}

/// Hash reference suitable for embedding in a future [`crate::witness::Witness`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AttestationRef {
    /// SHA-256 of the attestation file bytes.
    pub sha256: String,
    /// Optional filename at production time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// Provider capable of producing attestations over trace hashes.
pub trait AttestationProvider {
    /// Provider family this implementation emits.
    fn kind(&self) -> &'static str;

    /// Produce an attestation whose target is `trace_sha256`.
    ///
    /// # Errors
    /// Provider-specific failure, such as signing, token exchange, or
    /// hardware attestation failure.
    fn attest(&self, trace_sha256: &str) -> anyhow::Result<Attestation>;
}

/// Provider capable of verifying attestations.
pub trait AttestationVerifier {
    /// Provider family this implementation verifies.
    fn kind(&self) -> &'static str;

    /// Verify provider-specific proof material.
    ///
    /// # Errors
    /// Provider-specific IO or parser failure. Invalid proofs should
    /// return [`AttestationOutcome::Invalid`] rather than an error when
    /// verification completed normally.
    fn verify(&self, attestation: &Attestation) -> anyhow::Result<AttestationOutcome>;
}

/// Outcome of provider-specific attestation verification.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttestationOutcome {
    /// Provider proof verified.
    Trusted,
    /// Attestation kind is not handled by this verifier.
    UnsupportedKind { kind: String },
    /// Provider proof was checked and rejected.
    Invalid { reason: String },
}

impl AttestationOutcome {
    /// Whether this outcome confirms the provider claim.
    #[must_use]
    pub const fn is_trusted(&self) -> bool {
        matches!(self, Self::Trusted)
    }
}

impl std::fmt::Display for AttestationOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trusted => write!(f, "TRUSTED"),
            Self::UnsupportedKind { kind } => write!(f, "UNSUPPORTED_KIND kind={kind}"),
            Self::Invalid { reason } => write!(f, "INVALID reason={reason}"),
        }
    }
}

impl Attestation {
    /// Build a new attestation with the current schema version.
    #[must_use]
    pub fn new(
        kind: impl Into<String>,
        issuer: impl Into<String>,
        subject: impl Into<String>,
        target_sha256: impl Into<String>,
        freshness: Freshness,
        context: serde_json::Value,
        proof: serde_json::Value,
    ) -> Self {
        Self {
            version: ATTESTATION_VERSION,
            kind: kind.into(),
            issuer: issuer.into(),
            subject: subject.into(),
            context,
            target_sha256: target_sha256.into(),
            freshness,
            proof,
        }
    }

    /// Read an attestation from disk.
    ///
    /// # Errors
    /// IO error, JSON parse failure, or unsupported schema version.
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path.as_ref()).context("read attestation")?;
        Self::from_slice(&bytes)
    }

    /// Parse an attestation from JSON bytes.
    ///
    /// # Errors
    /// JSON parse failure or unsupported schema version.
    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let attestation: Self = serde_json::from_slice(bytes).context("parse attestation")?;
        anyhow::ensure!(
            attestation.version == ATTESTATION_VERSION,
            "attestation version {} not supported (expected {})",
            attestation.version,
            ATTESTATION_VERSION,
        );
        Ok(attestation)
    }

    /// Write the attestation as pretty-printed JSON with a trailing newline.
    ///
    /// # Errors
    /// IO error or serialization failure.
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        std::fs::write(path.as_ref(), self.to_json_bytes()?).context("write attestation")?;
        Ok(())
    }

    /// Serialize as pretty-printed JSON bytes with a trailing newline.
    ///
    /// # Errors
    /// Serialization failure.
    pub fn to_json_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json.into_bytes())
    }

    /// Whether this attestation targets `trace_sha256`.
    #[must_use]
    pub fn targets_trace(&self, trace_sha256: &str) -> bool {
        self.target_sha256 == trace_sha256
    }

    /// Hash reference for these serialized attestation bytes.
    #[must_use]
    pub fn reference_for_bytes(bytes: &[u8], filename: Option<String>) -> AttestationRef {
        AttestationRef {
            sha256: sha256_hex(bytes),
            filename,
        }
    }
}

impl AttestationRef {
    /// Build a reference to already-hashed attestation bytes.
    #[must_use]
    pub fn new(sha256: impl Into<String>, filename: Option<String>) -> Self {
        Self {
            sha256: sha256.into(),
            filename,
        }
    }
}

/// Built-in unsigned local-file provider.
///
/// This provider is useful for fixtures, demos, and detached plumbing
/// tests. It does not prove an external identity. It creates a sidecar
/// that can be hashed into a witness and checked for the critical binding:
/// `attestation.target_sha256 == witness.trace_sha256`.
#[derive(Debug, Clone)]
pub struct FileAttestationProvider {
    issuer: String,
    subject: String,
    freshness: Freshness,
    context: serde_json::Value,
}

impl FileAttestationProvider {
    /// Create an unsigned local-file provider.
    #[must_use]
    pub fn new(
        issuer: impl Into<String>,
        subject: impl Into<String>,
        freshness: Freshness,
        context: serde_json::Value,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            subject: subject.into(),
            freshness,
            context,
        }
    }
}

impl AttestationProvider for FileAttestationProvider {
    fn kind(&self) -> &'static str {
        FILE_ATTESTATION_KIND
    }

    fn attest(&self, trace_sha256: &str) -> anyhow::Result<Attestation> {
        Ok(Attestation::new(
            self.kind(),
            self.issuer.clone(),
            self.subject.clone(),
            trace_sha256,
            self.freshness.clone(),
            self.context.clone(),
            serde_json::json!({
                "algorithm": "none",
                "value": null,
                "warning": "unsigned local file anchor"
            }),
        ))
    }
}

/// Verifier for the built-in unsigned local-file provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileAttestationVerifier;

impl AttestationVerifier for FileAttestationVerifier {
    fn kind(&self) -> &'static str {
        FILE_ATTESTATION_KIND
    }

    fn verify(&self, attestation: &Attestation) -> anyhow::Result<AttestationOutcome> {
        if attestation.kind != self.kind() {
            return Ok(AttestationOutcome::UnsupportedKind {
                kind: attestation.kind.clone(),
            });
        }

        let algorithm = attestation
            .proof
            .get("algorithm")
            .and_then(serde_json::Value::as_str);
        if algorithm == Some("none") {
            Ok(AttestationOutcome::Trusted)
        } else {
            Ok(AttestationOutcome::Invalid {
                reason: "file attestation proof.algorithm must be \"none\"".into(),
            })
        }
    }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture() -> Attestation {
        Attestation::new(
            FILE_ATTESTATION_KIND,
            "test-suite",
            "fixture",
            "a".repeat(64),
            Freshness::Nonce {
                nonce: "nonce-1".into(),
            },
            json!({
                "path": "demo.ptytrace",
                "note": "unsigned fixture anchor"
            }),
            json!({
                "algorithm": "none",
                "value": null
            }),
        )
    }

    #[test]
    fn file_provider_emits_unsigned_anchor() {
        let provider = FileAttestationProvider::new(
            "issuer",
            "subject",
            Freshness::None,
            json!({ "path": "demo.ptytrace" }),
        );

        let attestation = provider.attest(&"a".repeat(64)).unwrap();

        assert_eq!(attestation.kind, FILE_ATTESTATION_KIND);
        assert_eq!(attestation.issuer, "issuer");
        assert_eq!(attestation.subject, "subject");
        assert!(attestation.targets_trace(&"a".repeat(64)));
        assert_eq!(attestation.proof["algorithm"], "none");
    }

    #[test]
    fn file_verifier_accepts_only_file_none_proof() {
        let verifier = FileAttestationVerifier;
        let mut attestation = fixture();

        assert!(verifier.verify(&attestation).unwrap().is_trusted());

        attestation.kind = "ssh".into();
        assert!(matches!(
            verifier.verify(&attestation).unwrap(),
            AttestationOutcome::UnsupportedKind { .. }
        ));

        attestation.kind = FILE_ATTESTATION_KIND.into();
        attestation.proof = json!({ "algorithm": "sha256-rsa" });
        assert!(matches!(
            verifier.verify(&attestation).unwrap(),
            AttestationOutcome::Invalid { .. }
        ));
    }

    #[test]
    fn constructor_sets_current_version() {
        let attestation = fixture();

        assert_eq!(attestation.version, ATTESTATION_VERSION);
    }

    #[test]
    fn attestation_round_trips_json_file() {
        let attestation = fixture();
        let tmp = tempfile::NamedTempFile::new().unwrap();

        attestation.write(tmp.path()).unwrap();
        let parsed = Attestation::read(tmp.path()).unwrap();

        assert_eq!(parsed, attestation);
    }

    #[test]
    fn read_rejects_wrong_version() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut attestation = fixture();
        attestation.version = ATTESTATION_VERSION + 1;
        attestation.write(tmp.path()).unwrap();

        let err = Attestation::read(tmp.path()).unwrap_err();

        assert!(err.to_string().contains("version"));
    }

    #[test]
    fn from_slice_rejects_wrong_version() {
        let mut attestation = fixture();
        attestation.version = ATTESTATION_VERSION + 1;
        let bytes = serde_json::to_vec(&attestation).unwrap();

        let err = Attestation::from_slice(&bytes).unwrap_err();

        assert!(err.to_string().contains("version"));
    }

    #[test]
    fn read_rejects_unknown_fields() {
        let json = r#"{
            "version": 1,
            "kind": "file",
            "issuer": "test-suite",
            "subject": "fixture",
            "context": {},
            "target_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "freshness": { "kind": "none" },
            "proof": {},
            "extra": true
        }"#;

        let err = serde_json::from_str::<Attestation>(json).unwrap_err();

        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn target_match_is_exact() {
        let attestation = fixture();

        assert!(attestation.targets_trace(&"a".repeat(64)));
        assert!(!attestation.targets_trace(&"b".repeat(64)));
    }

    #[test]
    fn reference_hashes_serialized_bytes() {
        let bytes = br#"{"version":1}"#;
        let reference = Attestation::reference_for_bytes(bytes, Some("anchor.json".into()));

        assert_eq!(
            reference.sha256,
            "2430f1a2ad2982d0067885488a4c89e21ad1d7c83b115ba8f1b20acc88dfaea8"
        );
        assert_eq!(reference.filename.as_deref(), Some("anchor.json"));
    }

    #[test]
    fn outcome_display_and_predicate_are_stable() {
        assert!(AttestationOutcome::Trusted.is_trusted());
        assert_eq!(AttestationOutcome::Trusted.to_string(), "TRUSTED");

        let invalid = AttestationOutcome::Invalid {
            reason: "bad signature".into(),
        };
        assert!(!invalid.is_trusted());
        assert_eq!(invalid.to_string(), "INVALID reason=bad signature");
    }
}
