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

/// Step dwell — the post-step interval before the next event.
///
/// Internally stored as `u64` nanoseconds so the recorder doesn't
/// throw away `Instant`'s native precision when converting from
/// wall-clock measurements. The asciinema v2 cast format encodes
/// timestamps as `f64` seconds, which has enough mantissa to hold
/// nanoseconds within a single recording session (`u64::MAX` ns is
/// ~584 years; `f64` keeps ~15.95 decimal digits, so 1 second of
/// recording resolves to ~10 ns and 100 hours to ~10 us).
///
/// Constructed via `from_duration` (lossless from `Instant` deltas)
/// or one of the unit-named factory methods. Pre-2026-05 versions
/// used a `Dwell(u32)` representation that quantized sub-ms input
/// timing to zero — see commit history for the migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Dwell(u64);

impl Dwell {
    /// Zero-length dwell. Used for the final event in a live capture
    /// (no "next event" to space against) and as the identity element.
    pub const ZERO: Self = Self(0);

    /// Construct from a raw nanosecond count. Prefer
    /// [`Dwell::from_duration`] when starting from a `Duration`.
    #[must_use]
    pub const fn from_nanos(ns: u64) -> Self {
        Self(ns)
    }

    /// Construct from microseconds. Saturates on overflow.
    #[must_use]
    pub const fn from_micros(us: u64) -> Self {
        Self(us.saturating_mul(1_000))
    }

    /// Construct from milliseconds. Saturates on overflow.
    #[must_use]
    pub const fn from_millis(ms: u64) -> Self {
        Self(ms.saturating_mul(1_000_000))
    }

    /// Lossless conversion from a [`Duration`]. Saturates at
    /// `u64::MAX` nanoseconds, which is far beyond any sensible
    /// session length.
    #[must_use]
    pub fn from_duration(d: Duration) -> Self {
        Self(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    }

    /// Nanosecond count (raw storage).
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Truncating conversion to whole milliseconds (raw u64). Kept
    /// for callers that emit `dwell_ms` fields in JSON sidecars.
    #[must_use]
    pub const fn as_millis_u64(self) -> u64 {
        self.0 / 1_000_000
    }

    /// Saturating conversion to whole milliseconds in `u32` for
    /// JSON fields that have a fixed-width contract.
    #[must_use]
    pub const fn as_millis_u32(self) -> u32 {
        let ms = self.as_millis_u64();
        if ms > u32::MAX as u64 {
            u32::MAX
        } else {
            #[allow(clippy::cast_possible_truncation)]
            {
                ms as u32
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Step {
    kind: EventKind,
    data: String,
    dwell: Dwell,
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
    /// Pre-first-event dwell accumulator. `record_beat` with a
    /// nonzero dwell on an empty `steps` would otherwise discard
    /// the dwell (no `last_mut()` to extend) — the first event in
    /// live capture would lose its leading idle interval. Recorded
    /// nanoseconds here are added to `t_ns`'s starting value in
    /// `finish` so the first emitted event's `time_s` reflects any
    /// pre-event beats.
    leading_dwell_ns: u64,
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
        self.steps
            .iter()
            .filter(|s| matches!(s.kind, EventKind::Output))
            .count()
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
        dwell: Dwell,
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
        dwell: Dwell,
        predicate: Option<Predicate>,
    ) -> anyhow::Result<()> {
        // Input bytes describe the cause but are not emitted to the
        // trace (asciinema v2 includes only "o" output events here).
        // The parameter stays in the API for caller record-keeping
        // and for future "i" event emission if ever wanted.
        let _ = input.into();
        let output = output.into();
        self.record_inner(&output, dwell, predicate)
    }

    /// Record output observed without a corresponding input.
    ///
    /// # Errors
    /// Currently infallible.
    pub fn record_output(
        &mut self,
        output: impl Into<Vec<u8>>,
        dwell: Dwell,
    ) -> anyhow::Result<()> {
        let output = output.into();
        self.record_inner(&output, dwell, None)
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
        dwell: Dwell,
    ) -> anyhow::Result<()> {
        let output = output.into();
        self.record_inner(&output, dwell, None)
    }

    /// Record a terminal resize event.
    ///
    /// The data format is asciicast v2's `COLSxROWS` resize payload.
    ///
    /// # Errors
    /// Either dimension is zero.
    pub fn record_resize(&mut self, cols: u16, rows: u16, dwell: Dwell) -> anyhow::Result<()> {
        if cols == 0 || rows == 0 {
            anyhow::bail!("record_resize requires nonzero dimensions: {cols}x{rows}");
        }
        self.steps.push(Step {
            kind: EventKind::Resize,
            data: format!("{cols}x{rows}"),
            dwell,
        });
        Ok(())
    }

    /// Add `dwell` to the most recently recorded step's dwell time.
    /// No-op when the recording is empty or `dwell` is zero. Used to
    /// extend the visible-time of the previous frame without emitting
    /// a new event.
    ///
    /// # Errors
    /// Currently infallible.
    pub fn record_beat(&mut self, dwell: Dwell) -> anyhow::Result<()> {
        if dwell.as_nanos() == 0 {
            return Ok(());
        }
        if let Some(last) = self.steps.last_mut() {
            last.dwell = Dwell::from_nanos(last.dwell.as_nanos().saturating_add(dwell.as_nanos()));
        } else {
            // No previous step to extend — accumulate as leading
            // dwell so the first real event's timestamp picks up
            // this interval. Without this, a live capture's idle
            // gap before its first event would silently collapse to
            // zero (the cause-of-first-event timing would be lost).
            self.leading_dwell_ns = self.leading_dwell_ns.saturating_add(dwell.as_nanos());
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
        output: &[u8],
        dwell: Dwell,
        predicate: Option<Predicate>,
    ) -> anyhow::Result<()> {
        // Empty output is a beat, not a step — extend the previous
        // step's dwell instead of emitting a zero-byte event.
        if output.is_empty() {
            return self.record_beat(dwell);
        }

        let data = String::from_utf8_lossy(output).into_owned();
        // Snapshot the accumulator length BEFORE the append so we
        // can rebuild the pre-call state on predicate failure. The
        // builder is `&mut self`, so an Err return is the only
        // observable side-effect signal — leaving partial state in
        // `accumulated_text` would silently feed corrupted bytes
        // into subsequent predicate evaluations.
        let pre_len = self.accumulated_text.len();
        self.accumulated_text.push_str(&data);

        if let Some(pred) = predicate
            && !pred.check(&self.accumulated_text)
        {
            self.accumulated_text.truncate(pre_len);
            anyhow::bail!(
                "record_step_matching: predicate {pred:?} did not hold against captured output"
            );
        }

        self.steps.push(Step {
            kind: EventKind::Output,
            data,
            dwell,
        });
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
    /// Either dimension is zero.
    pub fn finish_synthetic(self, cols: u16, rows: u16) -> anyhow::Result<Recording> {
        self.finish(cols, rows)
    }

    /// Same as [`Self::finish_synthetic`] — see note there.
    ///
    /// # Errors
    /// Either dimension is zero.
    pub fn finish_screen(self, cols: u16, rows: u16) -> anyhow::Result<Recording> {
        self.finish(cols, rows)
    }

    fn finish(self, cols: u16, rows: u16) -> anyhow::Result<Recording> {
        if cols == 0 || rows == 0 {
            anyhow::bail!("trace header dimensions must be nonzero; got {cols}x{rows}");
        }
        let header = TraceHeader {
            version: 2,
            width: u32::from(cols),
            height: u32::from(rows),
            env: BTreeMap::from([
                ("TERM".into(), "xterm-256color".into()),
                ("SHELL".into(), "/bin/bash".into()),
            ]),
        };

        // Timestamp plateau policy: a zero-dwell step does not advance
        // `t_ns`, so two adjacent zero-dwell events emit the same
        // `time_s`. That's the same behavior asciinema players have
        // tolerated for years and downstream consumers (frame_replay,
        // ptyrecord transcript) already treat it as "two events at the
        // same instant," not a malformed trace. We deliberately do
        // NOT bump duplicates by 1 ns here — enforcing strict
        // monotonicity would silently mutate caller-supplied dwells.
        // The `PTYTRACE_DEBUG_PLATEAU` env var enables an
        // opt-in stderr trace for callers diagnosing unexpected
        // collisions.
        let mut events = Vec::with_capacity(self.steps.len());
        // `leading_dwell_ns` is the sum of any `record_beat` dwells
        // pushed before the first real step. Seeding `t_ns` with it
        // lets pre-event idle time appear in the first event's
        // `time_s` rather than silently collapsing to zero.
        let mut t_ns: u64 = self.leading_dwell_ns;
        let debug_plateau = std::env::var_os("PTYTRACE_DEBUG_PLATEAU").is_some();
        let mut last_t_ns: Option<u64> = None;
        for (idx, step) in self.steps.iter().enumerate() {
            if debug_plateau && last_t_ns == Some(t_ns) {
                eprintln!(
                    "[ptytrace] zero-dwell timestamp plateau at event {idx}: \
                     two consecutive events share time_s={}",
                    ns_to_seconds(t_ns)
                );
            }
            events.push(TraceEvent {
                time_s: ns_to_seconds(t_ns),
                kind: step.kind,
                data: step.data.clone(),
            });
            last_t_ns = Some(t_ns);
            t_ns = t_ns.saturating_add(step.dwell.as_nanos());
        }

        Ok(Recording {
            trace: Trace { header, events },
            markers: self.markers,
        })
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

fn ns_to_seconds(ns: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = ns as f64;
    n / 1_000_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_step_appends_event() {
        let mut b = TraceBuilder::new();
        b.record_step(b"a".to_vec(), b"A".to_vec(), Dwell::from_millis(10))
            .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        assert_eq!(rec.trace().events.len(), 1);
        assert_eq!(rec.trace().events[0].data, "A");
    }

    /// Documents the load-bearing dwell contract: a step's `dwell` is
    /// the duration AFTER its event before the next one (post-step
    /// interval). `finish()` emits each event at the cumulative sum of
    /// prior dwells, never including the current step's own dwell.
    ///
    /// Live-capture must defer recording by one event to honor this —
    /// see `pty::live::flush_pending` for the implementation. Pre-fix,
    /// live mode supplied "dwell since previous event" instead, which
    /// shifted every cast timestamp by one event.
    #[test]
    fn dwell_is_post_step_interval() {
        let mut b = TraceBuilder::new();
        b.record_output(b"A".to_vec(), Dwell::from_millis(100))
            .unwrap();
        b.record_output(b"B".to_vec(), Dwell::from_millis(50))
            .unwrap();
        b.record_output(b"C".to_vec(), Dwell::from_millis(0))
            .unwrap();

        let rec = b.finish_synthetic(80, 24).unwrap();
        let events = &rec.trace().events;
        assert_eq!(events.len(), 3);

        // A at t=0 (no prior dwells)
        assert!((events[0].time_s - 0.0).abs() < f64::EPSILON);
        // B at t = A's dwell = 0.100
        assert!((events[1].time_s - 0.100).abs() < f64::EPSILON);
        // C at t = A's + B's dwell = 0.150
        assert!((events[2].time_s - 0.150).abs() < f64::EPSILON);
    }

    /// Regression: pre-2026-05 the dwell was `u32` milliseconds, so
    /// any input cadence below 1ms got quantized to a zero gap and
    /// the cast timeline collapsed adjacent events to the same
    /// `time_s`. Nanosecond storage preserves the input precision.
    #[test]
    fn sub_millisecond_dwells_survive_finish() {
        let mut b = TraceBuilder::new();
        // 100us between events — under the old quantum.
        b.record_output(b"A".to_vec(), Dwell::from_micros(100))
            .unwrap();
        b.record_output(b"B".to_vec(), Dwell::from_micros(100))
            .unwrap();
        b.record_output(b"C".to_vec(), Dwell::ZERO).unwrap();

        let rec = b.finish_synthetic(80, 24).unwrap();
        let events = &rec.trace().events;
        assert_eq!(events.len(), 3);
        assert!(events[1].time_s > events[0].time_s);
        assert!(events[2].time_s > events[1].time_s);
        // 100us = 0.0001s; well above f64 mantissa noise.
        assert!((events[1].time_s - 0.000_1).abs() < 1e-9);
        assert!((events[2].time_s - 0.000_2).abs() < 1e-9);
    }

    #[test]
    fn record_beat_extends_previous_dwell() {
        let mut b = TraceBuilder::new();
        b.record_step(b"a".to_vec(), b"A".to_vec(), Dwell::from_millis(10))
            .unwrap();
        b.record_beat(Dwell::from_millis(5)).unwrap();
        b.record_step(b"b".to_vec(), b"B".to_vec(), Dwell::from_millis(10))
            .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        // Second event timestamp = first dwell (10) + beat (5) = 15ms.
        assert!((rec.trace().events[1].time_s - 0.015).abs() < f64::EPSILON);
    }

    /// Regression: `record_beat` (and `record_output(b"", dwell)`,
    /// which routes through it) on an empty `steps` had no
    /// `last_mut()` to extend, so the dwell was silently dropped.
    /// In live capture this collapsed any pre-first-event idle gap
    /// to zero. The fix accumulates the dwell in
    /// `leading_dwell_ns` and seeds `t_ns` with it in `finish`, so
    /// the first emitted event's `time_s` reflects the leading
    /// interval.
    #[test]
    fn record_beat_before_first_step_advances_first_event_time() {
        let mut b = TraceBuilder::new();
        // No prior step — pre-fix the 100 ms is dropped.
        b.record_output(Vec::new(), Dwell::from_millis(100))
            .unwrap();
        // First real event. Its `time_s` must equal the leading dwell.
        b.record_output(b"X".to_vec(), Dwell::ZERO).unwrap();

        let rec = b.finish_synthetic(80, 24).unwrap();
        let events = &rec.trace().events;
        assert_eq!(events.len(), 1, "leading beat must not emit an event");
        assert!(
            (events[0].time_s - 0.100).abs() < f64::EPSILON,
            "first event time_s ({}) must include the 100 ms leading beat",
            events[0].time_s,
        );
    }

    /// Multiple leading beats accumulate.
    #[test]
    fn multiple_leading_beats_sum_into_first_event_time() {
        let mut b = TraceBuilder::new();
        b.record_beat(Dwell::from_millis(30)).unwrap();
        b.record_beat(Dwell::from_millis(70)).unwrap();
        b.record_output(b"X".to_vec(), Dwell::ZERO).unwrap();

        let rec = b.finish_synthetic(80, 24).unwrap();
        assert!((rec.trace().events[0].time_s - 0.100).abs() < f64::EPSILON);
    }

    #[test]
    fn record_resize_emits_asciicast_resize_event() {
        let mut b = TraceBuilder::new();
        b.record_output(b"first".to_vec(), Dwell::from_millis(50))
            .unwrap();
        b.record_resize(100, 30, Dwell::from_millis(25)).unwrap();
        b.record_output(b"second".to_vec(), Dwell::from_millis(10))
            .unwrap();

        let rec = b.finish_synthetic(80, 24).unwrap();

        assert_eq!(rec.trace().events.len(), 3);
        assert_eq!(rec.trace().events[1].kind, EventKind::Resize);
        assert!((rec.trace().events[1].time_s - 0.05).abs() < f64::EPSILON);
        assert_eq!(rec.trace().events[1].data, "100x30");
        assert!((rec.trace().events[2].time_s - 0.075).abs() < f64::EPSILON);
    }

    #[test]
    fn finish_rejects_zero_header_dimensions() {
        let err = TraceBuilder::new()
            .finish_synthetic(0, 24)
            .unwrap_err()
            .to_string();

        assert!(err.contains("nonzero"));
    }

    #[test]
    fn predicate_failure_returns_error() {
        let mut b = TraceBuilder::new();
        let err = b
            .record_step_matching(
                Vec::new(),
                b"hello".to_vec(),
                Dwell::from_millis(1),
                Some(Predicate::ContainsText {
                    text: "WORLD".into(),
                }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("predicate"));
    }

    /// Regression: a failed predicate previously left the matched-
    /// against bytes in `accumulated_text`, so a subsequent call's
    /// predicate would see the rolled-back step's bytes as if they
    /// had been recorded. The fix snapshots
    /// `accumulated_text.len()` pre-append and truncates on
    /// predicate failure.
    ///
    /// Test shape: feed "hello" against a predicate that fails
    /// (looking for "WORLD"). On the next call, assert
    /// `DoesNotContainText { text: "hello" }` passes against
    /// payload "X". Post-fix the accumulator is "X" (no "hello") so
    /// the predicate passes. Pre-fix the accumulator was "helloX"
    /// and the predicate would have failed — the assertion is
    /// load-bearing for the rollback.
    #[test]
    fn predicate_failure_rolls_back_accumulated_text() {
        let mut b = TraceBuilder::new();
        let _ = b
            .record_step_matching(
                Vec::new(),
                b"hello".to_vec(),
                Dwell::from_millis(1),
                Some(Predicate::ContainsText {
                    text: "WORLD".into(),
                }),
            )
            .unwrap_err();

        // Pre-fix this would Err — "helloX" contains "hello".
        // Post-fix this passes — accumulator is just "X".
        b.record_step_matching(
            Vec::new(),
            b"X".to_vec(),
            Dwell::from_millis(1),
            Some(Predicate::DoesNotContainText {
                text: "hello".into(),
            }),
        )
        .unwrap();
    }

    #[test]
    fn predicate_pass_records_step() {
        let mut b = TraceBuilder::new();
        b.record_step_matching(
            Vec::new(),
            b"hello world".to_vec(),
            Dwell::from_millis(1),
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
        b.record_step(b"a".to_vec(), b"A".to_vec(), Dwell::from_millis(10))
            .unwrap();
        b.record_step(Vec::new(), Vec::new(), Dwell::from_millis(7))
            .unwrap();
        let rec = b.finish_synthetic(80, 24).unwrap();
        assert_eq!(rec.trace().events.len(), 1);
    }

    #[test]
    fn presentation_output_emitted_in_trace() {
        let mut b = TraceBuilder::new();
        b.record_presentation_output(b"# note".to_vec(), Dwell::from_millis(10))
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
