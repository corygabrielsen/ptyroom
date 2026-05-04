//! Replay-verified semantic traces.
//!
//! A trace expectation names an intent, the raw log span that should realize
//! it, the predicate that must hold after replay, and the presentation dwell
//! assigned to that verified transition.

#![allow(clippy::module_name_repetitions)]

use std::collections::BTreeSet;
use std::marker::PhantomData;

use anyhow::{bail, ensure};
use serde::{Deserialize, Serialize};

use crate::observer::{Fact, Observer, Predicate};
use crate::proof::{DwellMs, IntentId, ProofState, StateHash, Unverified, Verified};
use crate::raw_log::{ByteBuf, RawLog, RawSpan};

/// Expected semantic transition before replay verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceExpectation {
    intent: IntentId,
    span: RawSpan,
    predicate: Predicate,
    dwell: DwellMs,
}

impl TraceExpectation {
    #[must_use]
    pub const fn new(
        intent: IntentId,
        span: RawSpan,
        predicate: Predicate,
        dwell: DwellMs,
    ) -> Self {
        Self {
            intent,
            span,
            predicate,
            dwell,
        }
    }

    #[must_use]
    pub const fn intent(&self) -> IntentId {
        self.intent
    }

    #[must_use]
    pub const fn span(&self) -> RawSpan {
        self.span
    }

    #[must_use]
    pub const fn predicate(&self) -> &Predicate {
        &self.predicate
    }

    #[must_use]
    pub const fn dwell(&self) -> DwellMs {
        self.dwell
    }

    /// Add presentation dwell to this expectation.
    ///
    /// # Errors
    /// Returns an error if the dwell sum overflows.
    pub fn add_dwell(&mut self, dwell: DwellMs) -> anyhow::Result<()> {
        self.dwell = self
            .dwell
            .checked_add(dwell)
            .ok_or_else(|| anyhow::anyhow!("dwell overflow for intent {}", self.intent.get()))?;
        Ok(())
    }
}

/// One verified semantic transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    intent: IntentId,
    span: RawSpan,
    before: StateHash,
    after: StateHash,
    predicate: Predicate,
    dwell: DwellMs,
    output: ByteBuf,
    facts: Vec<Fact>,
}

impl Transition {
    #[must_use]
    pub const fn intent(&self) -> IntentId {
        self.intent
    }

    #[must_use]
    pub const fn span(&self) -> RawSpan {
        self.span
    }

    #[must_use]
    pub const fn before(&self) -> StateHash {
        self.before
    }

    #[must_use]
    pub const fn after(&self) -> StateHash {
        self.after
    }

    #[must_use]
    pub const fn predicate(&self) -> &Predicate {
        &self.predicate
    }

    #[must_use]
    pub const fn dwell(&self) -> DwellMs {
        self.dwell
    }

    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    #[must_use]
    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }
}

/// Semantic trace parameterized by verification state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace<State: ProofState> {
    expectations: Vec<TraceExpectation>,
    transitions: Vec<Transition>,
    _state: PhantomData<State>,
}

impl Trace<Unverified> {
    #[must_use]
    pub fn from_expectations(expectations: Vec<TraceExpectation>) -> Self {
        Self {
            expectations,
            transitions: Vec::new(),
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn expectations(&self) -> &[TraceExpectation] {
        &self.expectations
    }

    /// Replay raw output through `observer` and check every expectation.
    ///
    /// # Errors
    /// Returns an error if spans are overlapping, a span has no output, a
    /// predicate fails, or any output event is not covered by the expectations.
    pub fn verify<O: Observer>(
        self,
        log: &RawLog<crate::proof::Closed>,
        observer: &mut O,
    ) -> anyhow::Result<Trace<Verified>> {
        let mut transitions = Vec::with_capacity(self.expectations.len());
        let mut covered_outputs = BTreeSet::new();
        let mut previous_end = None;

        for expectation in self.expectations {
            if let Some(end) = previous_end {
                ensure!(
                    end < expectation.span.start(),
                    "trace spans must be strictly ordered and non-overlapping"
                );
            }
            previous_end = Some(expectation.span.end());

            let before = observer.state().hash();
            let mut output = Vec::new();

            for event in log
                .events()
                .iter()
                .filter(|event| expectation.span.contains(event.seq()))
            {
                if event.direction().is_visible_output() {
                    output.extend_from_slice(event.bytes());
                    observer.apply_output(event.bytes());
                    covered_outputs.insert(event.seq());
                }
            }

            ensure!(
                !output.is_empty(),
                "intent {} span {}..{} produced no output",
                expectation.intent.get(),
                expectation.span.start().get(),
                expectation.span.end().get()
            );

            let after_state = observer.state();
            if !expectation.predicate.matches(&after_state) {
                bail!(
                    "intent {} predicate failed after span {}..{}",
                    expectation.intent.get(),
                    expectation.span.start().get(),
                    expectation.span.end().get()
                );
            }

            transitions.push(Transition {
                intent: expectation.intent,
                span: expectation.span,
                before,
                after: after_state.hash(),
                predicate: expectation.predicate,
                dwell: expectation.dwell,
                output,
                facts: after_state.facts(),
            });
        }

        for event in log.output_events() {
            ensure!(
                covered_outputs.contains(&event.seq()),
                "output event {} was not covered by any trace expectation",
                event.seq().get()
            );
        }

        Ok(Trace {
            expectations: Vec::new(),
            transitions,
            _state: PhantomData,
        })
    }
}

impl Trace<Verified> {
    #[must_use]
    pub fn transitions(&self) -> &[Transition] {
        &self.transitions
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.transitions.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::SyntheticObserver;
    use crate::proof::{Closed, Open, Seq};
    use crate::raw_log::RawLog;

    fn log_with_two_outputs() -> (RawLog<Closed>, RawSpan, RawSpan) {
        let mut log = RawLog::<Open>::new();
        let hello = log.append_output(b"hello ".to_vec());
        let world = log.append_output(b"world".to_vec());
        (log.close(), RawSpan::single(hello), RawSpan::single(world))
    }

    #[test]
    fn verifies_ordered_expectations_against_observer() {
        let (log, hello, world) = log_with_two_outputs();
        let trace = Trace::<Unverified>::from_expectations(vec![
            TraceExpectation::new(
                IntentId::new(1),
                hello,
                Predicate::ContainsText {
                    text: "hello".into(),
                },
                DwellMs::new(10),
            ),
            TraceExpectation::new(
                IntentId::new(2),
                world,
                Predicate::ContainsText {
                    text: "hello world".into(),
                },
                DwellMs::new(20),
            ),
        ]);

        let verified = trace.verify(&log, &mut SyntheticObserver::new()).unwrap();

        assert_eq!(verified.len(), 2);
        assert_eq!(verified.transitions()[0].output(), b"hello ");
        assert_eq!(verified.transitions()[1].dwell(), DwellMs::new(20));
    }

    #[test]
    fn rejects_unsatisfied_predicate() {
        let (log, hello, _) = log_with_two_outputs();
        let trace = Trace::<Unverified>::from_expectations(vec![TraceExpectation::new(
            IntentId::new(1),
            hello,
            Predicate::ContainsText {
                text: "missing".into(),
            },
            DwellMs::new(10),
        )]);

        let err = trace
            .verify(&log, &mut SyntheticObserver::new())
            .unwrap_err();
        assert!(err.to_string().contains("predicate failed"));
    }

    #[test]
    fn rejects_uncovered_output() {
        let (log, hello, _) = log_with_two_outputs();
        let trace = Trace::<Unverified>::from_expectations(vec![TraceExpectation::new(
            IntentId::new(1),
            hello,
            Predicate::ContainsText {
                text: "hello".into(),
            },
            DwellMs::new(10),
        )]);

        let err = trace
            .verify(&log, &mut SyntheticObserver::new())
            .unwrap_err();
        assert!(err.to_string().contains("not covered"));
    }

    #[test]
    fn rejects_overlapping_spans() {
        let mut log = RawLog::<Open>::new();
        log.append_output(b"one".to_vec());
        log.append_output(b"two".to_vec());
        let log = log.close();
        let overlapping = RawSpan::new(Seq::new(0), Seq::new(1)).unwrap();
        let second = RawSpan::single(Seq::new(1));
        let trace = Trace::<Unverified>::from_expectations(vec![
            TraceExpectation::new(
                IntentId::new(1),
                overlapping,
                Predicate::ContainsText { text: "one".into() },
                DwellMs::new(1),
            ),
            TraceExpectation::new(
                IntentId::new(2),
                second,
                Predicate::ContainsText { text: "two".into() },
                DwellMs::new(1),
            ),
        ]);

        let err = trace
            .verify(&log, &mut SyntheticObserver::new())
            .unwrap_err();
        assert!(err.to_string().contains("non-overlapping"));
    }
}
