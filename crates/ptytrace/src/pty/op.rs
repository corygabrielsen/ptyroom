//! Fluent builder over `PtyTracer` for the watch+write+capture chain.
//!
//! The recorder exposes three primitive operations that show up together
//! at almost every call site:
//!   1. arm a content-aware watch (`PtyTracer::arm_watch`)
//!   2. write bytes to the PTY (`PtyTracer::write_bytes`)
//!   3. capture the resulting output (`PtyTracer::capture_after` or a
//!      `WatchHandle::wait`)
//!
//! The ordering is load-bearing: the watch *must* be armed before the
//! write that causes the pattern, otherwise the bytes can arrive during
//! the settle window and be drained before the watch is in place. The
//! primitive API trusts the caller to get the order right; the builder
//! enforces it by construction.
//!
//! The underlying methods stay public as escape hatches — `PtyOp` is a
//! convenience layer, not a replacement.

use std::time::Duration;

use anyhow::Context;

use super::PtyTracer;
use super::drainer::{WatchHandle, escape_bytes};
use super::room_protocol::find_subslice;
use super::terminal_io::write_all;
use crate::recording::Dwell;

/// Result of an `expect` terminal: bytes captured up to and including
/// the matched pattern, plus the wall-clock time from arming to firing.
#[derive(Debug, Clone)]
pub struct WaitedCapture {
    /// Bytes drained up to and including the pattern's end. Any trailing
    /// bytes after the pattern are pushed back to the drainer for the
    /// next operation, matching `PtyTracer::send_raw_wait_for`'s
    /// partition-determinism cutoff.
    pub bytes: Vec<u8>,
    /// Wall-clock duration from `arm_watch` to the pattern firing.
    pub elapsed: Duration,
}

/// Fluent builder over the watch+write+capture chain.
///
/// Construct via [`PtyTracer::op`]. Chain `.watch(pattern)` and
/// `.write(bytes)` in any order; both are optional. Terminate with
/// `.capture(settle)` for a plain settle-then-drain, or `.expect(...)`
/// when a `.watch(...)` is armed and you want to block on it.
///
/// Examples:
///
/// ```no_run
/// use std::time::Duration;
/// use ptytrace::pty::{PtyTracer, PtyTracerConfig};
///
/// let mut rec = PtyTracer::spawn(PtyTracerConfig::default(), &["bash"])?;
///
/// // Plain write + settle.
/// let _bytes = rec.op().write(b"echo hi\n".to_vec()).capture(Duration::from_millis(5))?;
///
/// // Content-aware sync: arm watch, write trigger, block until match.
/// let waited = rec
///     .op()
///     .watch(b"$ ")
///     .write(b"true\n".to_vec())
///     .expect(Duration::from_secs(2), "prompt")?;
/// let _bytes = waited.bytes;
/// let _elapsed = waited.elapsed;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct PtyOp<'a> {
    rec: &'a mut PtyTracer,
    armed: Option<ArmedWatch>,
    pending_write: Option<Vec<u8>>,
}

/// Watch handle paired with the pattern bytes that armed it. The
/// pattern is kept alongside the handle so `expect` can split the
/// post-pattern leftover back to the drainer without the caller
/// passing the pattern twice.
struct ArmedWatch {
    handle: WatchHandle,
    pattern: Vec<u8>,
}

impl<'a> PtyOp<'a> {
    pub(super) fn new(rec: &'a mut PtyTracer) -> Self {
        Self {
            rec,
            armed: None,
            pending_write: None,
        }
    }

    /// Arm a content-aware watch on `pattern`. The drainer begins
    /// scanning *immediately* (before any subsequent `.write(...)`),
    /// preserving the arm-before-trigger ordering required by
    /// [`PtyTracer::arm_watch`].
    ///
    /// At most one watch may be armed per builder. A second call
    /// replaces the first.
    #[must_use]
    pub fn watch(mut self, pattern: &[u8]) -> Self {
        let pattern = pattern.to_vec();
        let handle = self.rec.arm_watch(&pattern);
        self.armed = Some(ArmedWatch { handle, pattern });
        self
    }

    /// Buffer bytes to be written when the builder terminates. The
    /// write does not happen until `.capture(...)` or `.expect(...)`
    /// is called, which guarantees any armed watch is registered with
    /// the drainer first.
    #[must_use]
    pub fn write(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.pending_write = Some(bytes.into());
        self
    }

    /// Terminate: flush any pending write, sleep `settle`, then drain
    /// the PTY buffer. Drained bytes are appended to the trace at zero
    /// playback dwell (so wait-style polls do not inflate virtual time)
    /// and also returned to the caller for inspection.
    ///
    /// If a watch was armed via `.watch(...)`, it is discarded here —
    /// callers that need to block on a pattern should use `.expect(...)`.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`, or PTY write failed.
    pub fn capture(self, settle: Duration) -> anyhow::Result<Vec<u8>> {
        let PtyOp {
            rec,
            armed,
            pending_write,
        } = self;
        // Drop the watch handle; the drainer entry will be cleaned up
        // when the pattern eventually fires (or the recorder is dropped).
        drop(armed);

        rec.check_runtime()?;
        if let Some(bytes) = pending_write
            && !bytes.is_empty()
        {
            write_all(rec.pty_fd(), &bytes)?;
        }
        std::thread::sleep(settle);
        let captured = rec.drainer().consume();
        if !captured.is_empty() {
            rec.recording_mut()
                .record_output(captured.clone(), Dwell::from_duration(Duration::ZERO))?;
        }
        Ok(captured)
    }

    /// Terminate: flush any pending write, then block until the armed
    /// watch fires or `timeout` elapses. Captured bytes up to and
    /// including the pattern become a single trace event at zero
    /// playback dwell; any trailing bytes after the pattern are pushed
    /// back to the drainer for the next operation.
    ///
    /// # Errors
    /// No watch armed, recording exceeded `max_runtime`, PTY write
    /// failed, or `timeout` elapsed without the pattern matching.
    pub fn expect(self, timeout: Duration, label: &str) -> anyhow::Result<WaitedCapture> {
        let PtyOp {
            rec,
            armed,
            pending_write,
        } = self;
        let ArmedWatch {
            handle: watch,
            pattern,
        } = armed.context("PtyOp::expect requires a prior .watch(pattern) call")?;

        rec.check_runtime()?;
        if let Some(bytes) = pending_write
            && !bytes.is_empty()
        {
            write_all(rec.pty_fd(), &bytes)?;
        }
        let elapsed = watch.wait(timeout).ok_or_else(|| {
            anyhow::anyhow!(
                "{label} timed out after {}ms waiting for {}",
                timeout.as_millis(),
                escape_bytes(&pattern),
            )
        })?;
        let captured = rec.drainer().consume();
        let pattern_end =
            find_subslice(&captured, &pattern).map_or(captured.len(), |i| i + pattern.len());
        let (this_event, leftover) = captured.split_at(pattern_end);
        let this_event = this_event.to_vec();
        if !leftover.is_empty() {
            rec.drainer().unconsume(leftover.to_vec());
        }
        if !this_event.is_empty() {
            rec.recording_mut()
                .record_output(this_event.clone(), Dwell::from_duration(Duration::ZERO))?;
        }
        Ok(WaitedCapture {
            bytes: this_event,
            elapsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::pty::{PtyTracer, PtyTracerConfig};

    /// `.write(...).capture(settle)` round-trips bytes through a `cat`
    /// child and records a trace event.
    #[test]
    fn write_then_capture_round_trips_through_cat() {
        let mut rec =
            PtyTracer::spawn(PtyTracerConfig::default(), &["cat"]).expect("spawn cat under pty");
        let captured = rec
            .op()
            .write(b"hello\n".to_vec())
            .capture(Duration::from_millis(50))
            .expect("capture cat echo");
        assert!(
            captured.windows(5).any(|w| w == b"hello"),
            "expected 'hello' in captured output, got {captured:?}",
        );
        // Output was recorded as a trace event.
        assert!(rec.event_count() >= 1);
        let _trace = rec.stop().expect("stop recorder");
    }

    /// `.watch(pattern).write(...).expect(timeout, label)` blocks until
    /// the pattern fires, splits post-pattern bytes back to the drainer,
    /// and returns the matched prefix plus elapsed time.
    #[test]
    fn watch_then_expect_blocks_until_pattern_fires() {
        let mut rec = PtyTracer::spawn(
            PtyTracerConfig::default(),
            &["sh", "-c", "printf 'pre-READY-post'"],
        )
        .expect("spawn sh under pty");
        let waited = rec
            .op()
            .watch(b"READY")
            .write(Vec::<u8>::new())
            .expect(Duration::from_secs(2), "ready marker")
            .expect("expect READY");
        assert!(
            waited.bytes.ends_with(b"READY"),
            "captured event should end at pattern, got {:?}",
            waited.bytes,
        );
        assert!(waited.elapsed <= Duration::from_secs(2));
        let _trace = rec.stop().expect("stop recorder");
    }

    /// `.expect(...)` without a prior `.watch(...)` returns an error
    /// rather than panicking.
    #[test]
    fn expect_without_watch_errors() {
        let mut rec =
            PtyTracer::spawn(PtyTracerConfig::default(), &["cat"]).expect("spawn cat under pty");
        let result = rec
            .op()
            .write(b"x".to_vec())
            .expect(Duration::from_millis(10), "no-watch");
        assert!(
            result.is_err(),
            "expected error from .expect without .watch"
        );
        let _trace = rec.stop().expect("stop recorder");
    }
}
