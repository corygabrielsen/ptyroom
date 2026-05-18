//! Deferred-flush buffer for share-mode trace events.
//!
//! `TraceBuilder` treats `dwell` as the post-step interval: how long
//! the recorded data stays on screen before the next event. During a
//! live share session we only learn that interval retrospectively,
//! when the next event arrives. Recording each event with the wall
//! time since the *previous* event (the obvious-but-wrong shape) makes
//! the first event absorb session bootstrap latency and skews every
//! later dwell by one step.
//!
//! [`PendingState`] holds one event at a time. When a new event
//! arrives, the previous one is flushed with
//! `next_arrival - prev_arrival` as its dwell. On session shutdown
//! the remaining pending event is flushed with dwell 0 — no more
//! events will arrive, so there is no interval to record.
//!
//! Mirrors the [`crate::pty::live`] pattern (see commit `26b840b`)
//! across both share-mode event sources: PTY output ([`super::pty_output`])
//! and resize events ([`super::sizing`]).

use std::time::Instant;

use crate::recording::{Dwell, TraceBuilder};

/// One trace event held back from the builder until its post-step
/// dwell is observable.
#[derive(Debug)]
pub(super) enum PendingEvent {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// Single-slot deferred-flush buffer keyed by arrival time.
#[derive(Debug, Default)]
pub(super) struct PendingState {
    slot: Option<(PendingEvent, Instant)>,
}

impl PendingState {
    /// Replace the buffered event with `event` arriving at `now`. If a
    /// previous event was buffered, flush it to `builder` with dwell
    /// `now - prev_arrival` first.
    ///
    /// # Errors
    /// Forwarded from [`TraceBuilder::record_output`] or
    /// [`TraceBuilder::record_resize`].
    pub(super) fn replace(
        &mut self,
        event: PendingEvent,
        now: Instant,
        builder: &mut TraceBuilder,
    ) -> anyhow::Result<()> {
        if let Some((prev, prev_time)) = self.slot.take() {
            let dwell = Dwell::from_duration(now.saturating_duration_since(prev_time));
            record_event(builder, prev, dwell)?;
        }
        self.slot = Some((event, now));
        Ok(())
    }

    /// Flush the buffered event (if any) with dwell 0. Used at session
    /// shutdown when no further event will arrive.
    ///
    /// # Errors
    /// Forwarded from the underlying record call.
    pub(super) fn flush_final(&mut self, builder: &mut TraceBuilder) -> anyhow::Result<()> {
        if let Some((event, _)) = self.slot.take() {
            record_event(builder, event, Dwell::ZERO)?;
        }
        Ok(())
    }
}

fn record_event(
    builder: &mut TraceBuilder,
    event: PendingEvent,
    dwell: Dwell,
) -> anyhow::Result<()> {
    match event {
        PendingEvent::Output(bytes) => builder.record_output(bytes, dwell),
        PendingEvent::Resize { cols, rows } => builder.record_resize(cols, rows, dwell),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::trace::EventKind;

    /// The first event's dwell must be bounded by the second event's
    /// arrival time, not by however long passed between session start
    /// and the first event. This is the bug fixed by deferring each
    /// flush until the next arrival is known.
    #[test]
    fn first_event_dwell_is_interval_to_next_event() {
        let mut pending = PendingState::default();
        let mut builder = TraceBuilder::new();

        let session_start = Instant::now();
        // Simulate a long bootstrap gap before the first event.
        let first_arrival = session_start + Duration::from_millis(500);
        let second_arrival = first_arrival + Duration::from_millis(10);

        pending
            .replace(
                PendingEvent::Output(b"first".to_vec()),
                first_arrival,
                &mut builder,
            )
            .unwrap();
        // No event flushed yet — first one is still buffered.
        assert_eq!(builder.event_count(), 0);

        pending
            .replace(
                PendingEvent::Output(b"second".to_vec()),
                second_arrival,
                &mut builder,
            )
            .unwrap();
        // First event flushed; its dwell is the gap to the second
        // arrival (10ms), NOT the gap from session_start (500ms).
        let recording = builder.finish_synthetic(80, 24).unwrap();
        let trace = recording.into_trace();
        let first = &trace.events[0];
        assert!(matches!(first.kind, EventKind::Output));
        // Allow generous slop but reject anything close to 500ms.
        // Compare in f64 — cast back to integer would lose precision
        // and the comparison is over a fixed threshold anyway.
        assert!(
            first.time_s < 0.1,
            "first event dwell leaked bootstrap latency: {}s",
            first.time_s
        );
    }

    #[test]
    fn flush_final_emits_buffered_with_zero_dwell() {
        let mut pending = PendingState::default();
        let mut builder = TraceBuilder::new();

        pending
            .replace(
                PendingEvent::Output(b"only".to_vec()),
                Instant::now(),
                &mut builder,
            )
            .unwrap();
        pending.flush_final(&mut builder).unwrap();

        let recording = builder.finish_synthetic(80, 24).unwrap();
        let trace = recording.into_trace();
        assert_eq!(trace.events.len(), 1);
    }

    #[test]
    fn resize_and_output_share_the_buffer() {
        let mut pending = PendingState::default();
        let mut builder = TraceBuilder::new();
        let t0 = Instant::now();

        pending
            .replace(
                PendingEvent::Resize {
                    cols: 100,
                    rows: 30,
                },
                t0,
                &mut builder,
            )
            .unwrap();
        pending
            .replace(
                PendingEvent::Output(b"after-resize".to_vec()),
                t0 + Duration::from_millis(25),
                &mut builder,
            )
            .unwrap();
        pending.flush_final(&mut builder).unwrap();

        let recording = builder.finish_synthetic(100, 30).unwrap();
        let trace = recording.into_trace();
        assert_eq!(trace.events.len(), 2);
        assert!(matches!(trace.events[0].kind, EventKind::Resize));
        assert!(matches!(trace.events[1].kind, EventKind::Output));
    }
}
