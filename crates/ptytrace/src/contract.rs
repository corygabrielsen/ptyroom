//! Trace specifications: post-hoc behavioral attestations.
//!
//! A [`Contract`] is a JSON sidecar carrying a list of [`Predicate`]s
//! that are expected to hold against the trace's accumulated output
//! text. [`Contract::check`] replays the trace in memory and reports
//! per-predicate pass/fail.
//!
//! This is the "C" half of the (B) reproducibility-receipt /
//! (C) trace-as-spec split: B says *who produced* this artifact and
//! that it is bit-exact; C says *what behavior* the artifact
//! exhibits. The two compose — a receipt can carry a spec hash so
//! verification covers both provenance and behavior.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::observer::Predicate;
use crate::trace::{EventKind, Trace};

/// Current schema version.
pub const SPEC_VERSION: u32 = 1;

/// Behavioral attestation against a trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Contract {
    /// Schema version; must equal [`SPEC_VERSION`].
    pub version: u32,
    /// Predicates that must hold against the trace's accumulated output.
    pub predicates: Vec<Predicate>,
}

impl Contract {
    /// Construct an empty spec.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            version: SPEC_VERSION,
            predicates: Vec::new(),
        }
    }

    /// Add a predicate. Returns `self` for chaining.
    #[must_use]
    pub fn with(mut self, predicate: Predicate) -> Self {
        self.predicates.push(predicate);
        self
    }

    /// Read a spec from disk.
    ///
    /// # Errors
    /// IO error or JSON parse failure; or schema-version mismatch.
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path.as_ref()).context("read spec")?;
        let spec: Self = serde_json::from_slice(&bytes).context("parse spec JSON")?;
        spec.validate().context("validate spec")?;
        Ok(spec)
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == SPEC_VERSION,
            "spec version {} not supported (expected {})",
            self.version,
            SPEC_VERSION,
        );
        Ok(())
    }

    /// Canonical on-disk byte representation. Pretty-printed JSON via
    /// [`serde_json::to_string_pretty`] plus a single trailing `\n`.
    ///
    /// This is the form that contract files MUST take. Anything that
    /// hashes a contract (witness `contract_sha256`, B+C composition,
    /// fixture diffs) MUST hash these bytes — never the raw bytes of an
    /// arbitrary on-disk file, since whitespace, key ordering changes
    /// between `serde_json` versions, or a hand-edit reflowing the JSON
    /// would otherwise change the hash without changing meaning.
    ///
    /// Pretty form (not compact) is chosen so contract files stay
    /// human-editable; the trailing newline + the `serde_json`-pinned
    /// formatter give the determinism guarantee.
    ///
    /// # Errors
    /// Serialization failure (in practice unreachable for valid specs).
    pub fn canonical_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let mut json = serde_json::to_string_pretty(self).context("serialize spec")?;
        json.push('\n');
        Ok(json.into_bytes())
    }

    /// Write the spec to disk in its [`canonical_bytes`] form.
    ///
    /// # Errors
    /// IO error or serialization failure.
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        std::fs::write(path.as_ref(), self.canonical_bytes()?).context("write spec")?;
        Ok(())
    }

    /// Replay `trace` and check each predicate against the
    /// UTF-8-lossy accumulation of all `"o"` (output) event bodies.
    ///
    /// Predicate semantics match record-time evaluation in
    /// [`crate::recording::TraceBuilder::record_step_matching`]
    /// — same haystack, same `check`, so a spec built from the same
    /// predicates that gated recording always passes verification.
    ///
    /// ```
    /// use ptytrace::trace::{Trace, TraceEvent, TraceHeader, EventKind};
    /// use ptytrace::observer::Predicate;
    /// use ptytrace::contract::Contract;
    ///
    /// let trace = Trace {
    ///     header: TraceHeader { version: 2, width: 80, height: 24, env: Default::default() },
    ///     events: vec![TraceEvent {
    ///         time_s: 0.0,
    ///         kind: EventKind::Output,
    ///         data: "hello world".into(),
    ///     }],
    /// };
    /// let spec = Contract::new()
    ///     .with(Predicate::ContainsText { text: "hello".into() })
    ///     .with(Predicate::DoesNotContainText { text: "error".into() });
    /// let report = spec.check(&trace);
    /// assert!(report.all_passed());
    /// ```
    #[must_use]
    pub fn check(&self, trace: &Trace) -> ContractReport {
        let mut accumulated = String::new();
        for event in &trace.events {
            if matches!(event.kind, EventKind::Output) {
                accumulated.push_str(&event.data);
            }
        }
        let outcomes = self
            .predicates
            .iter()
            .map(|p| {
                if p.check(&accumulated) {
                    CheckOutcome::Pass(p.clone())
                } else {
                    CheckOutcome::Fail(p.clone())
                }
            })
            .collect();
        ContractReport { outcomes }
    }
}

impl Default for Contract {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize on-disk spec bytes to the canonical hashing form: trim
/// trailing whitespace (spaces, tabs, CR, LF), then append exactly one
/// `\n`. Hashing the result is insensitive to whether the file was
/// written by [`Contract::write`] or hand-edited with a different
/// trailing-newline convention.
#[must_use]
pub fn canonicalize_bytes(bytes: &[u8]) -> Vec<u8> {
    let trimmed_len = bytes
        .iter()
        .rposition(|b| !matches!(*b, b' ' | b'\t' | b'\r' | b'\n'))
        .map_or(0, |i| i + 1);
    let mut out = Vec::with_capacity(trimmed_len + 1);
    out.extend_from_slice(&bytes[..trimmed_len]);
    out.push(b'\n');
    out
}

/// Result of one predicate evaluated against a trace.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CheckOutcome {
    Pass(Predicate),
    Fail(Predicate),
}

impl CheckOutcome {
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Pass(_))
    }

    #[must_use]
    pub const fn predicate(&self) -> &Predicate {
        match self {
            Self::Pass(p) | Self::Fail(p) => p,
        }
    }
}

impl std::fmt::Display for CheckOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (tag, p) = match self {
            Self::Pass(p) => ("PASS", p),
            Self::Fail(p) => ("FAIL", p),
        };
        write!(f, "{tag} {p:?}")
    }
}

/// Per-predicate verdict from a [`Contract::check`] call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ContractReport {
    pub outcomes: Vec<CheckOutcome>,
}

impl ContractReport {
    /// `true` iff every predicate in the spec passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.outcomes.iter().all(CheckOutcome::passed)
    }

    /// Count of failed predicates.
    #[must_use]
    pub fn failed_count(&self) -> usize {
        self.outcomes.iter().filter(|o| !o.passed()).count()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::trace::{TraceEvent, TraceHeader};

    use super::*;

    fn trace_with(output: &str) -> Trace {
        Trace {
            header: TraceHeader {
                version: 2,
                width: 80,
                height: 24,
                env: BTreeMap::new(),
            },
            events: vec![TraceEvent {
                time_s: 0.0,
                kind: EventKind::Output,
                data: output.into(),
            }],
        }
    }

    #[test]
    fn passing_spec_reports_all_pass() {
        let trace = trace_with("hello world");
        let spec = Contract::new()
            .with(Predicate::ContainsText {
                text: "hello".into(),
            })
            .with(Predicate::DoesNotContainText {
                text: "error".into(),
            });
        let report = spec.check(&trace);
        assert!(report.all_passed());
        assert_eq!(report.failed_count(), 0);
    }

    #[test]
    fn failing_predicate_reports_fail() {
        let trace = trace_with("hello world");
        let spec = Contract::new().with(Predicate::ContainsText {
            text: "missing".into(),
        });
        let report = spec.check(&trace);
        assert!(!report.all_passed());
        assert_eq!(report.failed_count(), 1);
        assert!(matches!(report.outcomes[0], CheckOutcome::Fail(_)));
    }

    #[test]
    fn input_events_ignored() {
        // Input events shouldn't affect predicate evaluation —
        // predicates assert what the user *sees*, not what was typed.
        let mut trace = trace_with("");
        trace.events.push(TraceEvent {
            time_s: 1.0,
            kind: EventKind::Input,
            data: "secret".into(),
        });
        let spec = Contract::new().with(Predicate::DoesNotContainText {
            text: "secret".into(),
        });
        let report = spec.check(&trace);
        assert!(report.all_passed());
    }

    #[test]
    fn read_distinguishes_parse_from_validate_errors() {
        // Malformed JSON: error chain must say "parse spec JSON", not
        // mention "validate".
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"{ not json").unwrap();
        let err = Contract::read(tmp.path()).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("parse spec JSON"),
            "expected parse context, got: {chain}"
        );
        assert!(
            !chain.contains("validate spec"),
            "parse error must not claim validation: {chain}"
        );

        // Well-formed JSON, wrong version: error chain must say
        // "validate spec", not mention "parse".
        let tmp2 = tempfile::NamedTempFile::new().unwrap();
        let bad_version = format!(r#"{{"version":{},"predicates":[]}}"#, SPEC_VERSION + 1);
        std::fs::write(tmp2.path(), bad_version).unwrap();
        let err = Contract::read(tmp2.path()).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("validate spec"),
            "expected validate context, got: {chain}"
        );
        assert!(
            !chain.contains("parse spec JSON"),
            "validation error must not claim a JSON parse failure: {chain}"
        );
    }

    #[test]
    fn canonicalize_bytes_normalizes_trailing_newline() {
        // No trailing newline → one appended.
        assert_eq!(canonicalize_bytes(b"abc"), b"abc\n");
        // Exactly one trailing newline → unchanged.
        assert_eq!(canonicalize_bytes(b"abc\n"), b"abc\n");
        // Multiple trailing newlines → collapsed to one.
        assert_eq!(canonicalize_bytes(b"abc\n\n\n"), b"abc\n");
        // CRLF / mixed trailing whitespace → collapsed to one \n.
        assert_eq!(canonicalize_bytes(b"abc\r\n"), b"abc\n");
        assert_eq!(canonicalize_bytes(b"abc \t\r\n"), b"abc\n");
        // Empty input → just a newline.
        assert_eq!(canonicalize_bytes(b""), b"\n");
        // All-whitespace input → just a newline.
        assert_eq!(canonicalize_bytes(b"\n\n\n"), b"\n");
    }

    #[test]
    fn canonicalize_matches_to_json_bytes_for_written_spec() {
        // A contract written by `Contract::write` must hash identically
        // to canonicalize_bytes applied to the file contents — the
        // round-trip render→verify path depends on it.
        let spec = Contract::new().with(Predicate::ContainsText { text: "hi".into() });
        let tmp = tempfile::NamedTempFile::new().unwrap();
        spec.write(tmp.path()).unwrap();
        let on_disk = std::fs::read(tmp.path()).unwrap();
        assert_eq!(
            canonicalize_bytes(&on_disk),
            spec.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn empty_spec_passes_trivially() {
        let trace = trace_with("anything");
        let spec = Contract::new();
        let report = spec.check(&trace);
        assert!(report.all_passed());
    }

    #[test]
    fn canonical_bytes_round_trip_is_stable() {
        // Build a non-trivial contract whose JSON shape exercises both
        // string and number scalars and a non-empty predicate array, so
        // the canonical form is something subsequent serde_json versions
        // could plausibly format differently in pretty mode.
        let original = Contract::new()
            .with(Predicate::ContainsText {
                text: "hello".into(),
            })
            .with(Predicate::DoesNotContainText {
                text: "error".into(),
            });
        let bytes = original.canonical_bytes().unwrap();
        // Trailing newline is part of the canonical form, and the
        // canonicalize_bytes helper agrees with Contract::canonical_bytes
        // on this — they're the same canonical form.
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(canonicalize_bytes(&bytes), bytes);
        // Round-trip: parse, re-serialize, expect bit-identical bytes.
        let parsed: Contract = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.canonical_bytes().unwrap(), bytes);
    }

    #[test]
    fn write_then_read_then_canonical_bytes_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("spec.json");
        let spec = Contract::new().with(Predicate::ContainsText { text: "x".into() });
        spec.write(&path).unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(on_disk, spec.canonical_bytes().unwrap());
        let round_tripped = Contract::read(&path).unwrap();
        assert_eq!(
            round_tripped.canonical_bytes().unwrap(),
            spec.canonical_bytes().unwrap()
        );
    }
}
