//! Trace specifications: post-hoc behavioral attestations.
//!
//! A [`Contract`] is a JSON sidecar carrying a list of [`Predicate`]s
//! that are expected to hold against the cast's accumulated output
//! text. [`Contract::check`] replays the cast in memory and reports
//! per-predicate pass/fail.
//!
//! This is the "C" half of the (B) reproducibility-receipt /
//! (C) cast-as-spec split: B says *who produced* this artifact and
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

/// Behavioral attestation against a cast.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Contract {
    /// Schema version; must equal [`SPEC_VERSION`].
    pub version: u32,
    /// Predicates that must hold against the cast's accumulated output.
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

    /// Write the spec to disk as pretty-printed JSON with a trailing
    /// newline.
    ///
    /// # Errors
    /// IO error or serialization failure.
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        std::fs::write(path.as_ref(), json).context("write spec")?;
        Ok(())
    }

    /// Replay `cast` and check each predicate against the
    /// UTF-8-lossy accumulation of all `"o"` (output) event bodies.
    ///
    /// Predicate semantics match record-time evaluation in
    /// [`crate::recording::TraceBuilder::record_step_matching`]
    /// — same haystack, same `check`, so a spec built from the same
    /// predicates that gated recording always passes verification.
    ///
    /// ```
    /// use tracer::trace::{Trace, TraceEvent, TraceHeader, EventKind};
    /// use tracer::observer::Predicate;
    /// use tracer::contract::Contract;
    ///
    /// let cast = Trace {
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
    /// let report = spec.check(&cast);
    /// assert!(report.all_passed());
    /// ```
    #[must_use]
    pub fn check(&self, cast: &Trace) -> ContractReport {
        let mut accumulated = String::new();
        for event in &cast.events {
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

/// Result of one predicate evaluated against a cast.
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

    fn cast_with(output: &str) -> Trace {
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
        let cast = cast_with("hello world");
        let spec = Contract::new()
            .with(Predicate::ContainsText {
                text: "hello".into(),
            })
            .with(Predicate::DoesNotContainText {
                text: "error".into(),
            });
        let report = spec.check(&cast);
        assert!(report.all_passed());
        assert_eq!(report.failed_count(), 0);
    }

    #[test]
    fn failing_predicate_reports_fail() {
        let cast = cast_with("hello world");
        let spec = Contract::new().with(Predicate::ContainsText {
            text: "missing".into(),
        });
        let report = spec.check(&cast);
        assert!(!report.all_passed());
        assert_eq!(report.failed_count(), 1);
        assert!(matches!(report.outcomes[0], CheckOutcome::Fail(_)));
    }

    #[test]
    fn input_events_ignored() {
        // Input events shouldn't affect predicate evaluation —
        // predicates assert what the user *sees*, not what was typed.
        let mut cast = cast_with("");
        cast.events.push(TraceEvent {
            time_s: 1.0,
            kind: EventKind::Input,
            data: "secret".into(),
        });
        let spec = Contract::new().with(Predicate::DoesNotContainText {
            text: "secret".into(),
        });
        let report = spec.check(&cast);
        assert!(report.all_passed());
    }

    #[test]
    fn empty_spec_passes_trivially() {
        let cast = cast_with("anything");
        let spec = Contract::new();
        let report = spec.check(&cast);
        assert!(report.all_passed());
    }
}
