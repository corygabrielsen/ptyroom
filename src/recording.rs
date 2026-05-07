//! Flat-list recording builder.
//!
//! Collects (input, output, dwell) tuples plus optional predicates,
//! and emits an asciinema v2 [`Trace`] on finish. Predicates run at
//! record time against the UTF-8-lossy accumulation of all output
//! bytes seen so far — a failing predicate halts recording with an
//! error so the caller can react before the recording is finalized.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::observer::Predicate;
use crate::trace::{EventKind, Trace, TraceEvent, TraceHeader};

/// Step dwell time in whole milliseconds.
///
/// Newtype to keep recorded dwell distinct from wall-clock durations
/// at API boundaries. Millisecond resolution matches asciinema v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DwellMs(u32);

impl DwellMs {
    #[must_use]
    pub const fn new(ms: u32) -> Self {
        Self(ms)
    }

    /// Saturating conversion from a [`Duration`] (truncates to ms).
    #[must_use]
    pub fn from_duration(d: Duration) -> Self {
        Self(u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone)]
struct Step {
    output: Vec<u8>,
    dwell: DwellMs,
}

/// Labeled instant relative to the start of recording.
#[derive(Debug, Clone, Serialize)]
pub struct TraceMarker {
    label: String,
    elapsed_ms: u64,
}

impl TraceMarker {
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }
}

/// Builds a [`Recording`] from incrementally captured steps.
#[derive(Debug, Default)]
pub struct TraceBuilder {
    steps: Vec<Step>,
    markers: Vec<TraceMarker>,
    /// UTF-8-lossy accumulation of all output bytes seen so far.
    /// Used as the haystack for `record_step_matching`'s predicate.
    accumulated_text: String,
}

impl TraceBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of recorded output events. Zero-output beats are not
    /// counted.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.steps.iter().filter(|s| !s.output.is_empty()).count()
    }

    /// Record a causal `(input, output)` pair. `dwell` is the
    /// post-step time the output should remain on screen.
    ///
    /// # Errors
    /// Currently infallible; returns `Result` for parity with
    /// [`Self::record_step_matching`] which can fail when its
    /// predicate fails.
    pub fn record_step(
        &mut self,
        input: impl Into<Vec<u8>>,
        output: impl Into<Vec<u8>>,
        dwell: DwellMs,
    ) -> anyhow::Result<()> {
        self.record_step_matching(input, output, dwell, None)
    }

    /// Record a step with an optional [`Predicate`] checked against
    /// the accumulated output text after this step's bytes are
    /// applied.
    ///
    /// # Errors
    /// Predicate evaluation returned false.
    pub fn record_step_matching(
        &mut self,
        input: impl Into<Vec<u8>>,
        output: impl Into<Vec<u8>>,
        dwell: DwellMs,
        predicate: Option<Predicate>,
    ) -> anyhow::Result<()> {
        // Input bytes describe the cause but are not emitted to the
        // trace (asciinema v2 includes only "o" output events here).
        // The parameter stays in the API for caller record-keeping
        // and for future "i" event emission if ever wanted.
        let _ = input.into();
        self.record_inner(output.into(), dwell, predicate)
    }

    /// Record output observed without a corresponding input.
    ///
    /// # Errors
    /// Currently infallible.
    pub fn record_output(
        &mut self,
        output: impl Into<Vec<u8>>,
        dwell: DwellMs,
    ) -> anyhow::Result<()> {
        self.record_inner(output.into(), dwell, None)
    }

    /// Record synthetic presentation output not produced by the child.
    /// Identical to [`Self::record_output`] in trace emission today;
    /// kept as a separate method for caller intent.
    ///
    /// # Errors
    /// Currently infallible.
    pub fn record_presentation_output(
        &mut self,
        output: impl Into<Vec<u8>>,
        dwell: DwellMs,
    ) -> anyhow::Result<()> {
        self.record_inner(output.into(), dwell, None)
    }

    /// Add `dwell` to the most recently recorded step's dwell time.
    /// No-op when the recording is empty or `dwell` is zero. Used to
    /// extend the visible-time of the previous frame without emitting
    /// a new event.
    ///
    /// # Errors
    /// Currently infallible.
    pub fn record_beat(&mut self, dwell: DwellMs) -> anyhow::Result<()> {
        if dwell.get() == 0 {
            return Ok(());
        }
        if let Some(last) = self.steps.last_mut() {
            last.dwell = DwellMs::new(last.dwell.get().saturating_add(dwell.get()));
        }
        Ok(())
    }

    /// Push a labeled marker at `elapsed` time-since-start.
    pub fn push_marker(&mut self, label: impl Into<String>, elapsed: Duration) {
        self.markers.push(TraceMarker {
            label: label.into(),
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        });
    }

    fn record_inner(
        &mut self,
        output: Vec<u8>,
        dwell: DwellMs,
        predicate: Option<Predicate>,
    ) -> anyhow::Result<()> {
        // Empty output is a beat, not a step — extend the previous
        // step's dwell instead of emitting a zero-byte event.
        if output.is_empty() {
            return self.record_beat(dwell);
        }

        self.accumulated_text
            .push_str(&String::from_utf8_lossy(&output));

        if let Some(pred) = predicate
            && !pred.check(&self.accumulated_text)
        {
            anyhow::bail!(
                "record_step_matching: predicate {pred:?} did not hold against captured output"
            );
        }

        self.steps.push(Step { output, dwell });
        Ok(())
    }

    /// Finish recording and produce a [`Recording`] sized for the
    /// given terminal dimensions.
    ///
    /// `finish_synthetic` and `finish_screen` produce identical traces
    /// today — historical naming is preserved so callers don't churn.
    /// The synthetic name was once distinguished from a vt100-rendering
    /// path; predicate evaluation now happens uniformly at record time.
    ///
    /// # Errors
    /// Currently infallible.
    pub fn finish_synthetic(self, cols: u16, rows: u16) -> anyhow::Result<Recording> {
        Ok(self.finish(cols, rows))
    }

    /// Same as [`Self::finish_synthetic`] — see note there.
    ///
    /// # Errors
    /// Currently infallible.
    pub fn finish_screen(self, cols: u16, rows: u16) -> anyhow::Result<Recording> {
        Ok(self.finish(cols, rows))
    }

    fn finish(self, cols: u16, rows: u16) -> Recording {
        let header = TraceHeader {
            version: 2,
            width: u32::from(cols),
            height: u32::from(rows),
            env: BTreeMap::from([
                ("TERM".into(), "xterm-256color".into()),
                ("SHELL".into(), "/bin/bash".into()),
            ]),
        };

        let mut events = Vec::new();
        let mut t_ms: u64 = 0;
        for step in &self.steps {
            if !step.output.is_empty() {
                events.push(TraceEvent {
                    time_s: ms_to_seconds(t_ms),
                    kind: EventKind::Output,
                    data: String::from_utf8_lossy(&step.output).into_owned(),
                });
            }
            t_ms = t_ms.saturating_add(u64::from(step.dwell.get()));
        }

        Recording {
            trace: Trace { header, events },
            markers: self.markers,
        }
    }
}

/// Finished recording artifact. Wraps a [`Trace`] plus optional
/// presentation markers.
#[derive(Debug, Clone)]
pub struct Recording {
    trace: Trace,
    markers: Vec<TraceMarker>,
}

impl Recording {
    #[must_use]
    pub const fn trace(&self) -> &Trace {
        &self.trace
    }

    #[must_use]
    pub fn markers(&self) -> &[TraceMarker] {
        &self.markers
    }

    #[must_use]
    pub fn into_trace(self) -> Trace {
        self.trace
    }
}

fn ms_to_seconds(ms: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = ms as f64;
    n / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_step_appends_event() {
        let mut b = TraceBuilder::new();
        b.record_step(b"a".to_vec(), b"A".to_vec(), DwellMs::new(10))
            .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        assert_eq!(rec.trace().events.len(), 1);
        assert_eq!(rec.trace().events[0].data, "A");
    }

    #[test]
    fn record_beat_extends_previous_dwell() {
        let mut b = TraceBuilder::new();
        b.record_step(b"a".to_vec(), b"A".to_vec(), DwellMs::new(10))
            .unwrap();
        b.record_beat(DwellMs::new(5)).unwrap();
        b.record_step(b"b".to_vec(), b"B".to_vec(), DwellMs::new(10))
            .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        // Second event timestamp = first dwell (10) + beat (5) = 15ms.
        assert!((rec.trace().events[1].time_s - 0.015).abs() < f64::EPSILON);
    }

    #[test]
    fn predicate_failure_returns_error() {
        let mut b = TraceBuilder::new();
        let err = b
            .record_step_matching(
                Vec::new(),
                b"hello".to_vec(),
                DwellMs::new(1),
                Some(Predicate::ContainsText {
                    text: "WORLD".into(),
                }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("predicate"));
    }

    #[test]
    fn predicate_pass_records_step() {
        let mut b = TraceBuilder::new();
        b.record_step_matching(
            Vec::new(),
            b"hello world".to_vec(),
            DwellMs::new(1),
            Some(Predicate::ContainsText {
                text: "hello".into(),
            }),
        )
        .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        assert_eq!(rec.trace().events.len(), 1);
    }

    #[test]
    fn empty_output_with_dwell_extends_previous() {
        let mut b = TraceBuilder::new();
        b.record_step(b"a".to_vec(), b"A".to_vec(), DwellMs::new(10))
            .unwrap();
        b.record_step(Vec::new(), Vec::new(), DwellMs::new(7))
            .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        assert_eq!(rec.trace().events.len(), 1);
    }

    #[test]
    fn presentation_output_emitted_in_trace() {
        let mut b = TraceBuilder::new();
        b.record_presentation_output(b"# note".to_vec(), DwellMs::new(10))
            .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        assert_eq!(rec.trace().events[0].data, "# note");
    }

    #[test]
    fn markers_attached() {
        let mut b = TraceBuilder::new();
        b.push_marker("start", Duration::from_millis(100));
        b.push_marker("end", Duration::from_millis(500));
        let rec = b.finish_synthetic(80, 24).unwrap();
        assert_eq!(rec.markers().len(), 2);
        assert_eq!(rec.markers()[0].label(), "start");
        assert_eq!(rec.markers()[1].elapsed_ms(), 500);
    }
}
