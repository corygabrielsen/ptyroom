//! Background reader for the PTY master fd.
//!
//! Three responsibilities:
//!  1. Continuously drain the master fd so the child process doesn't
//!     block on a full PTY buffer.
//!  2. Watch every drained chunk for OSC 11/10 queries and write the
//!     canned replies back through the master — the recorder is the
//!     terminal emulator from the child's perspective.
//!  3. Notify registered watches when their byte pattern appears in the
//!     stream (used by `Tracer::arm_watch` to replace fixed
//!     real-time padding with content-aware sync).
//!
//! Drained bytes accumulate in a thread-safe buffer; the parent atomically
//! swaps it out via `consume`.

use std::os::fd::{BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nix::sys::select::{FdSet, select};
use nix::sys::time::{TimeVal, TimeValLike};
use nix::unistd::{read, write};

use super::osc::{StubColors, replies_for_chunk, setters_in_chunk};

/// Bytes buffered by the drainer between consume calls, plus the list
/// of pattern watches that haven't fired yet. Single mutex so the
/// drainer can update both atomically per chunk.
#[derive(Default)]
struct State {
    bytes: Vec<u8>,
    watches: Vec<Watch>,
}

/// A pending pattern watch. Holds the pattern, a small carry-over
/// buffer to handle chunk-boundary spans, and the signal half of the
/// notification. One-shot: removed from the watch list once fired.
struct Watch {
    pattern: Vec<u8>,
    carry: Vec<u8>,
    signal: Arc<WatchSignal>,
}

/// Synchronization payload shared between the drainer (writer) and
/// `WatchHandle::wait` (reader).
struct WatchSignal {
    fired_at: Mutex<Option<Instant>>,
    notify: Condvar,
}

/// Caller-side handle returned by `Drainer::register_watch`. Holds the
/// pattern (for diagnostic logging) plus the synchronization payload.
pub struct WatchHandle {
    signal: Arc<WatchSignal>,
    started_at: Instant,
    pattern: Vec<u8>,
}

impl WatchHandle {
    /// Block until the pattern fires or `timeout` elapses. Returns the
    /// wall-time from arming to the pattern firing, or `None` on timeout.
    ///
    /// When `TRACER_PROFILE=1` is set, every wait (regardless
    /// of outcome) logs `pattern` + elapsed time to stderr, so a single
    /// run produces a tunable trace of every content-aware sync point.
    ///
    /// Consumes `self` because the watch is one-shot.
    ///
    /// # Panics
    /// Panics if the watch mutex or condvar has been poisoned.
    #[must_use]
    pub fn wait(self, timeout: Duration) -> Option<Duration> {
        let WatchSignal { fired_at, notify } = &*self.signal;
        let guard = fired_at.lock().expect("watch signal mutex poisoned");
        let (guard, _res) = notify
            .wait_timeout_while(guard, timeout, |fired_at| fired_at.is_none())
            .expect("watch condvar wait poisoned");
        let outcome = guard
            .as_ref()
            .map(|fired_at| fired_at.duration_since(self.started_at));
        if std::env::var_os("TRACER_PROFILE").is_some() {
            let elapsed_str = match outcome {
                Some(d) => format!("{}us", d.as_micros()),
                None => format!(">{}us TIMED OUT", timeout.as_micros()),
            };
            eprintln!(
                "[profile] watch {} fired in {}",
                escape_bytes(&self.pattern),
                elapsed_str,
            );
        }
        outcome
    }

    /// Borrow the pattern this handle is watching. Used by callers that
    /// want to construct their own error messages.
    #[must_use]
    pub fn pattern(&self) -> &[u8] {
        &self.pattern
    }
}

/// Render a byte slice with escape sequences readable: `\e` for ESC,
/// `\xNN` for other non-printables, printable ASCII as-is.
fn escape_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() + 2);
    s.push('"');
    for &b in bytes {
        match b {
            0x1b => s.push_str("\\e"),
            b'\\' => s.push_str("\\\\"),
            b'"' => s.push_str("\\\""),
            0x20..=0x7e => s.push(b as char),
            _ => write!(s, "\\x{b:02x}").expect("infallible String fmt"),
        }
    }
    s.push('"');
    s
}

pub struct Drainer {
    inner: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drainer {
    pub fn start(master_fd: RawFd, stubs: StubColors) -> Self {
        let inner: Arc<Mutex<State>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let buf = Arc::clone(&inner);
        let stop_flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("tracer-drainer".into())
            .spawn(move || drain_loop(master_fd, stubs, buf, stop_flag))
            .expect("drainer thread spawn");
        Self {
            inner,
            stop,
            thread: Some(thread),
        }
    }

    pub fn consume(&self) -> Vec<u8> {
        let mut s = self.inner.lock().expect("drainer mutex poisoned");
        std::mem::take(&mut s.bytes)
    }

    /// Push bytes back onto the front of the buffer so the next
    /// `consume` returns them first. Used by `send_raw_wait_for` to
    /// retain post-pattern bytes for the next event instead of folding
    /// them into the `wait_for` event.
    pub fn unconsume(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let mut s = self.inner.lock().expect("drainer mutex poisoned");
        if s.bytes.is_empty() {
            s.bytes = bytes;
        } else {
            let mut combined = bytes;
            combined.extend_from_slice(&s.bytes);
            s.bytes = combined;
        }
    }

    /// Register a pattern watch. The drainer scans every subsequent
    /// chunk for the pattern (with carry-over across chunk boundaries)
    /// and fires the returned handle on the first match.
    ///
    /// As a convenience for the common "send trigger then call this"
    /// race, the existing buffered bytes are scanned at registration
    /// time too — if the pattern is already present, the handle fires
    /// immediately.
    pub fn register_watch(&self, pattern: Vec<u8>) -> WatchHandle {
        assert!(!pattern.is_empty(), "register_watch: empty pattern");
        let signal = Arc::new(WatchSignal {
            fired_at: Mutex::new(None),
            notify: Condvar::new(),
        });
        let started_at = Instant::now();

        let mut s = self.inner.lock().expect("drainer mutex poisoned");

        // Did the pattern already arrive? Scan existing buffer once.
        if contains_pattern(&s.bytes, &pattern) {
            *signal.fired_at.lock().unwrap() = Some(started_at);
            signal.notify.notify_all();
        } else {
            s.watches.push(Watch {
                pattern: pattern.clone(),
                carry: Vec::new(),
                signal: Arc::clone(&signal),
            });
        }

        WatchHandle {
            signal,
            started_at,
            pattern,
        }
    }
}

impl Drop for Drainer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Search `haystack` for `needle`. Naive O(n*m) — patterns are short
/// (~10 bytes for OSC sequences) and chunks are bounded; no need for
/// a proper substring search algorithm.
fn contains_pattern(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// `Arc` parameters are moved in but only their refs are used inside the
// loop; the closure that spawns the thread is the actual consumer of the
// owned Arcs (it captures them).
//
// The `stubs` value is the *initial* state. The drainer updates it in place
// as it observes OSC 10/11 setter writes from the child, so subsequent
// query responses reflect the most recently set bg/fg. A child that sets
// the terminal bg/fg and later queries it should observe its own write.
#[allow(clippy::needless_pass_by_value)]
fn drain_loop(fd: RawFd, mut stubs: StubColors, state: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    let mut chunk = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::SeqCst) {
        let mut set = FdSet::new();
        // Safety: `fd` is the master end of a PTY whose lifetime exceeds
        // this thread (the parent `Tracer` joins us in `Drop` before
        // dropping the OwnedFd).
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        set.insert(borrowed);
        let mut tv = TimeVal::milliseconds(50);
        match select(Some(fd + 1), Some(&mut set), None, None, Some(&mut tv)) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(_) => break,
        }
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let n = match read(borrowed, &mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        };
        let read_slice = &chunk[..n];

        // Apply setter writes before answering queries — if a single chunk
        // contains both, the child expects the query to reflect the
        // just-set value.
        for (code, color) in setters_in_chunk(read_slice) {
            match code {
                10 => stubs.fg = color,
                11 => stubs.bg = color,
                _ => {}
            }
        }

        for reply in replies_for_chunk(read_slice, stubs) {
            let mut written = 0;
            while written < reply.len() {
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
                match write(borrowed, &reply[written..]) {
                    Ok(0) => break,
                    Ok(k) => written += k,
                    Err(nix::errno::Errno::EINTR) => {}
                    Err(_) => break,
                }
            }
        }

        if let Ok(mut s) = state.lock() {
            // Update watches BEFORE appending bytes so each watch's carry
            // captures only its own state (independent of buffer growth).
            update_watches(&mut s.watches, read_slice);
            s.bytes.extend_from_slice(read_slice);
        }
    }
}

/// Feed `chunk` to every pending watch. Watches whose pattern matched
/// are removed from the list and notified.
fn update_watches(watches: &mut Vec<Watch>, chunk: &[u8]) {
    if watches.is_empty() {
        return;
    }
    // Use indices so we can swap_remove fired watches without invalidating
    // iteration. Iterate in reverse so swap_remove doesn't shift the
    // not-yet-visited slots.
    let mut i = watches.len();
    while i > 0 {
        i -= 1;
        if watches[i].observe(chunk) {
            let w = watches.swap_remove(i);
            w.fire();
        }
    }
}

impl Watch {
    /// Search `chunk` plus this watch's carry-over for the pattern.
    /// Returns true on match. Updates `carry` to retain the suffix that
    /// could still be the start of the pattern.
    fn observe(&mut self, chunk: &[u8]) -> bool {
        // Concatenate carry || chunk. Allocates each time; chunks are
        // bounded so this is cheap.
        let mut search = Vec::with_capacity(self.carry.len() + chunk.len());
        search.extend_from_slice(&self.carry);
        search.extend_from_slice(chunk);

        if contains_pattern(&search, &self.pattern) {
            return true;
        }

        // Save the trailing (pattern.len - 1) bytes for the next chunk —
        // any shorter is enough, since a complete pattern wouldn't have
        // fit in those bytes anyway.
        let carry_len = self.pattern.len().saturating_sub(1);
        let new_carry_start = search.len().saturating_sub(carry_len);
        self.carry = search[new_carry_start..].to_vec();
        false
    }

    fn fire(self) {
        let WatchSignal { fired_at, notify } = &*self.signal;
        *fired_at.lock().expect("watch fire mutex poisoned") = Some(Instant::now());
        notify.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_finds_pattern_in_single_chunk() {
        let mut w = Watch {
            pattern: b"abc".to_vec(),
            carry: Vec::new(),
            signal: Arc::new(WatchSignal {
                fired_at: Mutex::new(None),
                notify: Condvar::new(),
            }),
        };
        assert!(w.observe(b"xxabcxx"));
    }

    #[test]
    fn observe_finds_pattern_across_chunks() {
        let mut w = Watch {
            pattern: b"abcdef".to_vec(),
            carry: Vec::new(),
            signal: Arc::new(WatchSignal {
                fired_at: Mutex::new(None),
                notify: Condvar::new(),
            }),
        };
        assert!(!w.observe(b"xxxabc"));
        assert_eq!(w.carry, b"xxabc"); // last 5 bytes (= pattern.len - 1)
        assert!(w.observe(b"def"));
    }

    #[test]
    fn observe_no_match_keeps_carry_bounded() {
        let mut w = Watch {
            pattern: b"AB".to_vec(),
            carry: Vec::new(),
            signal: Arc::new(WatchSignal {
                fired_at: Mutex::new(None),
                notify: Condvar::new(),
            }),
        };
        assert!(!w.observe(b"xxxxxxxxxxxxxxxxxxxx"));
        // pattern.len - 1 = 1, carry should be 1 byte.
        assert_eq!(w.carry.len(), 1);
    }
}
