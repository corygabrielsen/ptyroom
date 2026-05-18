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
        let spec: Self = serde_json::from_slice(&bytes).context("parse spec")?;
        anyhow::ensure!(
            spec.version == SPEC_VERSION,
            "spec version {} not supported (expected {})",
            spec.version,
            SPEC_VERSION,
        );
        Ok(spec)
    }

    /// Canonical on-disk byte representation. Compact JSON produced by
    /// [`serde_json::to_vec`] plus a single trailing `\n`.
    ///
    /// This is the form that contract files MUST take. Anything that
    /// hashes a contract (witness `contract_sha256`, B+C composition,
    /// fixture diffs) MUST hash these bytes — never the raw bytes of an
    /// arbitrary on-disk file, since whitespace, key ordering changes
    /// between `serde_json` versions, or a hand-edit reflowing the JSON
    /// would otherwise change the hash without changing meaning.
    ///
    /// # Errors
    /// Serialization failure (in practice unreachable for valid specs).
    pub fn canonical_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self).context("serialize spec")?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Write the spec to disk in its [`canonical_bytes`] form.
    ///
    /// # Errors
    /// IO error or serialization failure.
    ///
    /// [`canonical_bytes`]: Self::canonical_bytes
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let bytes = self.canonical_bytes()?;
        std::fs::write(path.as_ref(), bytes).context("write spec")?;
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
        // Trailing newline is part of the canonical form.
        assert_eq!(bytes.last(), Some(&b'\n'));
        // Compact form: no spaces between separators.
        assert!(
            !bytes.windows(2).any(|w| w == b", " || w == b": "),
            "canonical form must be compact JSON: {:?}",
            String::from_utf8_lossy(&bytes)
        );
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
