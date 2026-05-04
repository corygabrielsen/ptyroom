//! Monotonic presentation timeline derived from verified transitions.
//!
//! This compiler is deliberately small: verified causal transitions provide
//! output bytes plus dwell, and the compiler turns those into absolute
//! presentation timestamps. Capture time and machine scheduling do not enter
//! this layer.

use std::collections::BTreeMap;
use std::marker::PhantomData;

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

use crate::cast::{Cast, CastEvent, CastHeader, EventKind};
use crate::proof::{Monotonic, ProofState, TimestampMs, Verified};
use crate::raw_log::ByteBuf;
use crate::verified_trace::Trace;

/// Output bytes scheduled at an absolute presentation timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEvent {
    timestamp: TimestampMs,
    bytes: ByteBuf,
}

impl TimelineEvent {
    #[must_use]
    pub const fn timestamp(&self) -> TimestampMs {
        self.timestamp
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Presentation timeline parameterized by proof state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline<State: ProofState> {
    events: Vec<TimelineEvent>,
    _state: PhantomData<State>,
}

impl Timeline<Monotonic> {
    /// Compile a verified trace into monotonic presentation timestamps.
    ///
    /// # Errors
    /// Returns an error if dwell accumulation overflows.
    pub fn compile(trace: &Trace<Verified>) -> anyhow::Result<Self> {
        let mut events = Vec::new();
        let mut timestamp = TimestampMs::zero();
        let mut last_output_timestamp = None;

        for transition in trace.transitions() {
            if !transition.output().is_empty() {
                events.push(TimelineEvent {
                    timestamp,
                    bytes: transition.output().to_vec(),
                });
                last_output_timestamp = Some(timestamp);
            }

            timestamp = timestamp.checked_add(transition.dwell()).with_context(|| {
                format!(
                    "timeline timestamp overflow after intent {}",
                    transition.intent().get()
                )
            })?;
        }

        if let Some(last_output) = last_output_timestamp
            && timestamp > last_output
        {
            events.push(TimelineEvent {
                timestamp,
                bytes: Vec::new(),
            });
        }

        ensure!(
            is_monotonic(&events),
            "timeline compiler produced non-monotonic events"
        );
        Ok(Self {
            events,
            _state: PhantomData,
        })
    }

    #[must_use]
    pub fn events(&self) -> &[TimelineEvent] {
        &self.events
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[must_use]
    pub fn to_cast(&self, cols: u16, rows: u16) -> Cast {
        let header = CastHeader {
            version: 2,
            width: u32::from(cols),
            height: u32::from(rows),
            env: BTreeMap::from([
                ("TERM".into(), "xterm-256color".into()),
                ("SHELL".into(), "/bin/bash".into()),
            ]),
        };
        let events = self
            .events
            .iter()
            .map(|event| CastEvent {
                time_s: ms_to_seconds(event.timestamp.get()),
                kind: EventKind::Output,
                data: String::from_utf8_lossy(&event.bytes).into_owned(),
            })
            .collect();

        Cast { header, events }
    }
}

fn is_monotonic(events: &[TimelineEvent]) -> bool {
    events
        .windows(2)
        .all(|pair| pair[0].timestamp <= pair[1].timestamp)
}

#[allow(clippy::cast_precision_loss)]
fn ms_to_seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::{Predicate, SyntheticObserver};
    use crate::proof::{DwellMs, IntentId, Open, Unverified};
    use crate::raw_log::{RawLog, RawSpan};
    use crate::verified_trace::{Trace, TraceExpectation};

    fn verified_trace(dwells: &[u64]) -> Trace<Verified> {
        let mut log = RawLog::<Open>::new();
        let mut expectations = Vec::new();

        for (idx, dwell) in dwells.iter().enumerate() {
            let seq = log.append_output(format!("event-{idx};").into_bytes());
            expectations.push(TraceExpectation::new(
                IntentId::new(u64::try_from(idx).unwrap()),
                RawSpan::single(seq),
                Predicate::ContainsText {
                    text: format!("event-{idx};"),
                },
                DwellMs::new(*dwell),
            ));
        }

        Trace::<Unverified>::from_expectations(expectations)
            .verify(&log.close(), &mut SyntheticObserver::new())
            .unwrap()
    }

    #[test]
    fn compile_produces_monotonic_timestamps() {
        let trace = verified_trace(&[10, 20, 30]);
        let timeline = Timeline::<Monotonic>::compile(&trace).unwrap();
        let timestamps: Vec<_> = timeline
            .events()
            .iter()
            .map(|event| event.timestamp().get())
            .collect();

        assert_eq!(timestamps, vec![0, 10, 30, 60]);
    }

    #[test]
    fn compile_preserves_trailing_dwell_with_empty_output() {
        let trace = verified_trace(&[250]);
        let timeline = Timeline::<Monotonic>::compile(&trace).unwrap();

        assert_eq!(timeline.events().len(), 2);
        assert_eq!(timeline.events()[0].bytes(), b"event-0;");
        assert_eq!(timeline.events()[1].timestamp().get(), 250);
        assert!(timeline.events()[1].bytes().is_empty());
    }

    #[test]
    fn compile_rejects_timestamp_overflow() {
        let trace = verified_trace(&[u64::MAX, 1]);
        let err = Timeline::<Monotonic>::compile(&trace).unwrap_err();

        assert!(err.to_string().contains("overflow"));
    }

    #[test]
    fn cast_conversion_uses_millisecond_seconds() {
        let trace = verified_trace(&[125]);
        let cast = Timeline::<Monotonic>::compile(&trace)
            .unwrap()
            .to_cast(80, 24);

        assert_eq!(cast.header.width, 80);
        assert_eq!(cast.header.height, 24);
        assert_eq!(cast.events[0].data, "event-0;");
        assert!((cast.events[1].time_s - 0.125).abs() < f64::EPSILON);
    }
}
