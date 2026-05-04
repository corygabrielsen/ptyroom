//! Typestated raw IO evidence.
//!
//! `RawLog<Open>` is append-only. Closing it produces `RawLog<Closed>`, which
//! is immutable evidence for trace verification.

#![allow(clippy::module_name_repetitions)]

use std::marker::PhantomData;

use anyhow::ensure;
use serde::{Deserialize, Serialize};

use crate::proof::{Closed, Open, ProofState, Seq};

/// Owned bytes crossing the adapter boundary.
pub type ByteBuf = Vec<u8>;

/// Direction of a raw adapter event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Bytes written by the recorder to the child PTY.
    Input,
    /// Bytes captured from the child PTY.
    Output,
    /// Synthetic bytes inserted directly into presentation output.
    ///
    /// These bytes render in the cast but were not emitted by the child
    /// process. They are useful for labels, blank prompt lines, and other
    /// visual-only demo structure.
    PresentationOutput,
}

impl Direction {
    #[must_use]
    pub const fn is_visible_output(self) -> bool {
        matches!(self, Self::Output | Self::PresentationOutput)
    }
}

/// One ordered adapter-boundary event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEvent {
    seq: Seq,
    direction: Direction,
    bytes: ByteBuf,
}

impl RawEvent {
    #[must_use]
    pub const fn seq(&self) -> Seq {
        self.seq
    }

    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Inclusive span of raw event sequence numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSpan {
    start: Seq,
    end: Seq,
}

impl RawSpan {
    /// Build a raw span whose start is not after its end.
    ///
    /// # Errors
    /// Returns an error if `start > end`.
    pub fn new(start: Seq, end: Seq) -> anyhow::Result<Self> {
        ensure!(
            start <= end,
            "raw span start {} is after end {}",
            start.get(),
            end.get(),
        );
        Ok(Self { start, end })
    }

    #[must_use]
    pub const fn single(seq: Seq) -> Self {
        Self {
            start: seq,
            end: seq,
        }
    }

    #[must_use]
    pub const fn start(self) -> Seq {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> Seq {
        self.end
    }

    #[must_use]
    pub const fn contains(self, seq: Seq) -> bool {
        self.start.get() <= seq.get() && seq.get() <= self.end.get()
    }
}

/// Raw adapter evidence parameterized by lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawLog<State: ProofState> {
    events: Vec<RawEvent>,
    next_seq: u64,
    #[serde(skip)]
    _state: PhantomData<State>,
}

impl Default for RawLog<Open> {
    fn default() -> Self {
        Self::new()
    }
}

impl RawLog<Open> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: Vec::new(),
            next_seq: 0,
            _state: PhantomData,
        }
    }

    pub fn append_input(&mut self, bytes: impl Into<ByteBuf>) -> Seq {
        self.append(Direction::Input, bytes.into())
    }

    pub fn append_output(&mut self, bytes: impl Into<ByteBuf>) -> Seq {
        self.append(Direction::Output, bytes.into())
    }

    pub fn append_presentation_output(&mut self, bytes: impl Into<ByteBuf>) -> Seq {
        self.append(Direction::PresentationOutput, bytes.into())
    }

    #[must_use]
    pub fn close(self) -> RawLog<Closed> {
        RawLog {
            events: self.events,
            next_seq: self.next_seq,
            _state: PhantomData,
        }
    }

    fn append(&mut self, direction: Direction, bytes: ByteBuf) -> Seq {
        let seq = Seq::new(self.next_seq);
        self.next_seq = self.next_seq.saturating_add(1);
        self.events.push(RawEvent {
            seq,
            direction,
            bytes,
        });
        seq
    }
}

impl RawLog<Closed> {
    #[must_use]
    pub fn events(&self) -> &[RawEvent] {
        &self.events
    }

    #[must_use]
    pub fn event(&self, seq: Seq) -> Option<&RawEvent> {
        self.events.iter().find(|event| event.seq == seq)
    }

    pub fn output_events(&self) -> impl Iterator<Item = &RawEvent> {
        self.events
            .iter()
            .filter(|event| event.direction.is_visible_output())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_log_appends_monotonic_sequences() {
        let mut log = RawLog::<Open>::new();
        let a = log.append_input(b"a".to_vec());
        let b = log.append_output(b"b".to_vec());
        assert_eq!(a.get(), 0);
        assert_eq!(b.get(), 1);

        let closed = log.close();
        assert_eq!(closed.len(), 2);
        assert_eq!(closed.event(a).unwrap().direction(), Direction::Input);
        assert_eq!(closed.event(b).unwrap().bytes(), b"b");
    }

    #[test]
    fn closed_log_filters_outputs() {
        let mut log = RawLog::<Open>::new();
        log.append_input(b"in".to_vec());
        log.append_output(b"one".to_vec());
        log.append_presentation_output(b"two".to_vec());
        let closed = log.close();
        let outputs: Vec<_> = closed.output_events().map(RawEvent::bytes).collect();
        assert_eq!(outputs, vec![b"one".as_slice(), b"two".as_slice()]);
        assert_eq!(
            closed.event(Seq::new(2)).unwrap().direction(),
            Direction::PresentationOutput,
        );
    }

    #[test]
    fn raw_span_rejects_inverted_ranges() {
        let err = RawSpan::new(Seq::new(2), Seq::new(1)).unwrap_err();
        assert!(err.to_string().contains("after end"));
    }

    #[test]
    fn raw_span_contains_inclusive_endpoints() {
        let span = RawSpan::new(Seq::new(2), Seq::new(4)).unwrap();
        assert!(!span.contains(Seq::new(1)));
        assert!(span.contains(Seq::new(2)));
        assert!(span.contains(Seq::new(4)));
        assert!(!span.contains(Seq::new(5)));
    }
}
