//! Recorder-facing trace builder.
//!
//! This is the bridge between the product recorder and the typed lower
//! layers. Scene code records causal input/output pairs here; this builder
//! produces raw evidence, verifies ordered transitions, compiles a monotonic
//! presentation timeline, and finally exposes an asciicast.

use std::path::Path;

use serde::{Serialize, Serializer};

use crate::cast::Cast;
use crate::observer::{Fact, Observer, Predicate, ScreenObserver, SyntheticObserver};
use crate::proof::{Closed, DwellMs, IntentId, Monotonic, Open, Seq, StateHash, Verified};
use crate::proof_timeline::{Timeline, TimelineEvent};
use crate::raw_log::{ByteBuf, Direction, RawEvent, RawLog, RawSpan};
use crate::verified_trace::{Trace, TraceExpectation, Transition};

/// Incrementally builds a deterministic recording trace from captured IO.
#[derive(Debug, Clone)]
pub struct RecordingBuilder {
    raw_log: RawLog<Open>,
    expectations: Vec<TraceExpectation>,
    markers: Vec<RecordingMarker>,
    next_intent: u64,
    observed_output_count: u64,
}

impl Default for RecordingBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            raw_log: RawLog::new(),
            expectations: Vec::new(),
            markers: Vec::new(),
            next_intent: 0,
            observed_output_count: 0,
        }
    }

    #[must_use]
    pub fn event_count(&self) -> usize {
        self.expectations.len()
    }

    /// Record a causal input/output pair with the default ordering predicate.
    ///
    /// # Errors
    /// Returns an error if raw span construction or dwell accumulation fails.
    pub fn record_step(
        &mut self,
        input: impl Into<ByteBuf>,
        output: impl Into<ByteBuf>,
        dwell: DwellMs,
    ) -> anyhow::Result<Option<IntentId>> {
        self.record_step_matching(input, output, dwell, None)
    }

    /// Record a causal input/output pair with a caller-supplied predicate.
    ///
    /// # Errors
    /// Returns an error if raw span construction or dwell accumulation fails.
    pub fn record_step_matching(
        &mut self,
        input: impl Into<ByteBuf>,
        output: impl Into<ByteBuf>,
        dwell: DwellMs,
        predicate: Option<Predicate>,
    ) -> anyhow::Result<Option<IntentId>> {
        self.record_step_with_output_direction(input, output, dwell, predicate, Direction::Output)
    }

    fn record_step_with_output_direction(
        &mut self,
        input: impl Into<ByteBuf>,
        output: impl Into<ByteBuf>,
        dwell: DwellMs,
        predicate: Option<Predicate>,
        output_direction: Direction,
    ) -> anyhow::Result<Option<IntentId>> {
        let input = input.into();
        let output = output.into();

        let start = if input.is_empty() {
            None
        } else {
            Some(self.raw_log.append_input(input))
        };

        if output.is_empty() {
            self.record_beat(dwell)?;
            return Ok(None);
        }

        self.observed_output_count = self.observed_output_count.saturating_add(1);
        let output_seq = match output_direction {
            Direction::Output => self.raw_log.append_output(output),
            Direction::PresentationOutput => self.raw_log.append_presentation_output(output),
            Direction::Input => anyhow::bail!("input cannot be used as an output direction"),
        };
        let span = RawSpan::new(start.unwrap_or(output_seq), output_seq)?;
        let intent = self.next_intent();
        let predicate = predicate.unwrap_or(Predicate::EventCountIs {
            count: self.observed_output_count,
        });
        self.expectations
            .push(TraceExpectation::new(intent, span, predicate, dwell));
        Ok(Some(intent))
    }

    /// Record output with no associated input.
    ///
    /// # Errors
    /// Returns an error if raw span construction or dwell accumulation fails.
    pub fn record_output(
        &mut self,
        output: impl Into<ByteBuf>,
        dwell: DwellMs,
    ) -> anyhow::Result<Option<IntentId>> {
        self.record_step(Vec::new(), output, dwell)
    }

    /// Record synthetic presentation output with no associated child input.
    ///
    /// Presentation output renders in the cast, participates in snapshot
    /// verification, and is explicitly marked in the raw evidence log as not
    /// having come from the child PTY.
    ///
    /// # Errors
    /// Returns an error if raw span construction or dwell accumulation fails.
    pub fn record_presentation_output(
        &mut self,
        output: impl Into<ByteBuf>,
        dwell: DwellMs,
    ) -> anyhow::Result<Option<IntentId>> {
        self.record_step_with_output_direction(
            Vec::new(),
            output,
            dwell,
            None,
            Direction::PresentationOutput,
        )
    }

    /// Advance presentation time without new output.
    ///
    /// Pure beats are represented as extra dwell on the previous visible
    /// transition. A leading beat is ignored because no prior frame exists to
    /// hold on screen.
    ///
    /// # Errors
    /// Returns an error if dwell accumulation overflows.
    pub fn record_beat(&mut self, dwell: DwellMs) -> anyhow::Result<()> {
        if dwell.get() == 0 {
            return Ok(());
        }
        if let Some(last) = self.expectations.last_mut() {
            last.add_dwell(dwell)?;
        }
        Ok(())
    }

    pub fn push_marker(&mut self, label: impl Into<String>, elapsed: std::time::Duration) {
        self.markers.push(RecordingMarker {
            label: label.into(),
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        });
    }

    /// Verify and compile with the default synthetic observer.
    ///
    /// # Errors
    /// Returns an error if verification or timeline compilation fails.
    pub fn finish_synthetic(self, cols: u16, rows: u16) -> anyhow::Result<VerifiedRecording> {
        self.finish(cols, rows, &mut SyntheticObserver::new())
    }

    /// Verify and compile with a small terminal-like screen observer.
    ///
    /// # Errors
    /// Returns an error if verification or timeline compilation fails.
    pub fn finish_screen(self, cols: u16, rows: u16) -> anyhow::Result<VerifiedRecording> {
        self.finish(cols, rows, &mut ScreenObserver::new(cols, rows))
    }

    /// Verify and compile with a caller-supplied observer.
    ///
    /// # Errors
    /// Returns an error if verification or timeline compilation fails.
    pub fn finish<O: Observer>(
        self,
        cols: u16,
        rows: u16,
        observer: &mut O,
    ) -> anyhow::Result<VerifiedRecording> {
        let raw_log = self.raw_log.close();
        let trace = Trace::<crate::proof::Unverified>::from_expectations(self.expectations)
            .verify(&raw_log, observer)?;
        let timeline = Timeline::<Monotonic>::compile(&trace)?;
        let cast = timeline.to_cast(cols, rows);
        Ok(VerifiedRecording {
            raw_log,
            trace,
            timeline,
            cast,
            markers: self.markers,
        })
    }

    fn next_intent(&mut self) -> IntentId {
        let intent = IntentId::new(self.next_intent);
        self.next_intent = self.next_intent.saturating_add(1);
        intent
    }
}

/// Fully verified recording artifact.
#[derive(Debug, Clone)]
pub struct VerifiedRecording {
    raw_log: RawLog<Closed>,
    trace: Trace<Verified>,
    timeline: Timeline<Monotonic>,
    cast: Cast,
    markers: Vec<RecordingMarker>,
}

impl VerifiedRecording {
    #[must_use]
    pub const fn raw_log(&self) -> &RawLog<Closed> {
        &self.raw_log
    }

    #[must_use]
    pub const fn trace(&self) -> &Trace<Verified> {
        &self.trace
    }

    #[must_use]
    pub const fn timeline(&self) -> &Timeline<Monotonic> {
        &self.timeline
    }

    #[must_use]
    pub const fn cast(&self) -> &Cast {
        &self.cast
    }

    #[must_use]
    pub fn markers(&self) -> &[RecordingMarker] {
        &self.markers
    }

    #[must_use]
    pub fn into_cast(self) -> Cast {
        self.cast
    }

    /// Write a compact JSON artifact for inspection and regression fixtures.
    ///
    /// # Errors
    /// Returns an IO or JSON serialization error.
    pub fn write_json(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_json_string()?)?;
        Ok(())
    }

    /// Serialize this artifact as compact, inspection-friendly JSON.
    ///
    /// # Errors
    /// Returns a JSON serialization error.
    pub fn to_json_string(&self) -> anyhow::Result<String> {
        let view = RecordingArtifactView::from(self);
        Ok(serde_json::to_string_pretty(&view)?)
    }
}

/// Wall-clock diagnostic marker. Markers are never used for playback time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingMarker {
    label: String,
    elapsed_ms: u64,
}

impl RecordingMarker {
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }
}

#[derive(Serialize)]
struct RecordingArtifactView<'a> {
    raw_events: Vec<RawEventView<'a>>,
    transitions: Vec<TransitionView<'a>>,
    timeline_events: Vec<TimelineEventView<'a>>,
    markers: &'a [RecordingMarker],
}

impl<'a> From<&'a VerifiedRecording> for RecordingArtifactView<'a> {
    fn from(recording: &'a VerifiedRecording) -> Self {
        Self {
            raw_events: recording
                .raw_log
                .events()
                .iter()
                .map(RawEventView::from)
                .collect(),
            transitions: recording
                .trace
                .transitions()
                .iter()
                .map(TransitionView::from)
                .collect(),
            timeline_events: recording
                .timeline
                .events()
                .iter()
                .map(TimelineEventView::from)
                .collect(),
            markers: &recording.markers,
        }
    }
}

#[derive(Serialize)]
struct RawEventView<'a> {
    seq: Seq,
    direction: Direction,
    bytes: HexBytes<'a>,
}

impl<'a> From<&'a RawEvent> for RawEventView<'a> {
    fn from(event: &'a RawEvent) -> Self {
        Self {
            seq: event.seq(),
            direction: event.direction(),
            bytes: HexBytes(event.bytes()),
        }
    }
}

#[derive(Serialize)]
struct TransitionView<'a> {
    intent: IntentId,
    span: RawSpan,
    before: StateHash,
    after: StateHash,
    predicate: &'a Predicate,
    dwell: DwellMs,
    output: HexBytes<'a>,
    facts: &'a [Fact],
}

impl<'a> From<&'a Transition> for TransitionView<'a> {
    fn from(transition: &'a Transition) -> Self {
        Self {
            intent: transition.intent(),
            span: transition.span(),
            before: transition.before(),
            after: transition.after(),
            predicate: transition.predicate(),
            dwell: transition.dwell(),
            output: HexBytes(transition.output()),
            facts: transition.facts(),
        }
    }
}

#[derive(Serialize)]
struct TimelineEventView<'a> {
    timestamp: crate::proof::TimestampMs,
    bytes: HexBytes<'a>,
}

impl<'a> From<&'a TimelineEvent> for TimelineEventView<'a> {
    fn from(event: &'a TimelineEvent) -> Self {
        Self {
            timestamp: event.timestamp(),
            bytes: HexBytes(event.bytes()),
        }
    }
}

struct HexBytes<'a>(&'a [u8]);

impl Serialize for HexBytes<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(self.0.len() * 2);
        for byte in self.0 {
            write!(&mut out, "{byte:02x}").expect("infallible String fmt");
        }
        serializer.serialize_str(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_compiles_causal_steps_to_cast() {
        let mut builder = RecordingBuilder::new();
        builder
            .record_step_matching(
                b"a".to_vec(),
                b"a".to_vec(),
                DwellMs::new(10),
                Some(Predicate::ContainsText { text: "a".into() }),
            )
            .unwrap();
        builder
            .record_step(b"\n".to_vec(), b"\r\n$ ".to_vec(), DwellMs::new(20))
            .unwrap();

        let recording = builder.finish_synthetic(80, 24).unwrap();

        assert_eq!(recording.raw_log().len(), 4);
        assert_eq!(recording.trace().len(), 2);
        assert_eq!(recording.timeline().events()[1].timestamp().get(), 10);
        assert_eq!(recording.cast().events[0].data, "a");
    }

    #[test]
    fn markers_are_diagnostics_not_timeline_events() {
        let mut builder = RecordingBuilder::new();
        builder.push_marker("slow", std::time::Duration::from_secs(30));
        builder
            .record_output(b"one".to_vec(), DwellMs::new(10))
            .unwrap();

        let recording = builder.finish_synthetic(80, 24).unwrap();

        assert_eq!(recording.markers()[0].label(), "slow");
        assert_eq!(recording.timeline().events()[1].timestamp().get(), 10);
    }

    #[test]
    fn artifact_json_uses_hex_bytes() {
        let mut builder = RecordingBuilder::new();
        builder
            .record_step(b"a".to_vec(), b"\x1b[31m".to_vec(), DwellMs::new(1))
            .unwrap();
        let json = builder
            .finish_synthetic(80, 24)
            .unwrap()
            .to_json_string()
            .unwrap();

        assert!(json.contains(r#""bytes": "1b5b33316d""#));
        assert!(!json.contains(r#""bytes": ["#));
    }

    #[test]
    fn presentation_output_is_marked_in_raw_log() {
        let mut builder = RecordingBuilder::new();
        builder
            .record_presentation_output(b"# heading".to_vec(), DwellMs::new(10))
            .unwrap();

        let recording = builder.finish_synthetic(80, 24).unwrap();

        let event = recording.raw_log().events().first().unwrap();
        assert_eq!(event.direction(), Direction::PresentationOutput);
        assert_eq!(recording.cast().events[0].data, "# heading");
    }

    #[test]
    fn finish_screen_verifies_visible_terminal_text() {
        let mut builder = RecordingBuilder::new();
        builder
            .record_step_matching(
                Vec::new(),
                b"hello\r\nworld".to_vec(),
                DwellMs::new(1),
                Some(Predicate::ContainsText {
                    text: "hello\nworld".into(),
                }),
            )
            .unwrap();

        let recording = builder.finish_screen(20, 4).unwrap();

        assert_eq!(recording.trace().len(), 1);
    }

    #[test]
    fn pure_beat_extends_previous_visible_frame() {
        let mut builder = RecordingBuilder::new();
        builder
            .record_output(b"one".to_vec(), DwellMs::new(10))
            .unwrap();
        builder.record_beat(DwellMs::new(50)).unwrap();
        builder
            .record_output(b"two".to_vec(), DwellMs::new(5))
            .unwrap();

        let recording = builder.finish_synthetic(80, 24).unwrap();
        let times: Vec<_> = recording
            .timeline()
            .events()
            .iter()
            .map(|event| event.timestamp().get())
            .collect();

        assert_eq!(times, vec![0, 60, 65]);
    }

    #[test]
    fn empty_output_records_input_but_no_transition() {
        let mut builder = RecordingBuilder::new();
        assert!(
            builder
                .record_step(b"\x1b[B".to_vec(), Vec::new(), DwellMs::new(10))
                .unwrap()
                .is_none()
        );
        builder
            .record_output(b"later".to_vec(), DwellMs::new(1))
            .unwrap();

        let recording = builder.finish_synthetic(80, 24).unwrap();

        assert_eq!(recording.raw_log().len(), 2);
        assert_eq!(recording.trace().len(), 1);
    }
}
