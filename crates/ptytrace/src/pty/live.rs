//! Live, interactive terminal session recording.
//!
//! Spawns a child shell under a PTY, puts the host's stdin into raw
//! mode, and runs a foreground IO loop that
//!
//!   - forwards user keystrokes (stdin → PTY master),
//!   - tees PTY output to host stdout (so the user sees their session),
//!   - records each PTY chunk with a wall-clock dwell.
//!
//! The loop terminates when the PTY hits EOF (typically the user types
//! `exit` or hits Ctrl-D in the recorded shell), or when the recorder's
//! `max_runtime` budget is exhausted.
//!
//! **Determinism scope.** Unlike scripted recording (virtual playback
//! time, byte-stable trace under repetition), live recording uses real
//! wall-clock dwells: the trace's timeline is a record of what was
//! typed when, not a reproducible derivation. The downstream
//! `trace -> media` render remains byte-stable; receipts attest that
//! arrow.

use std::io::{self, IsTerminal};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{read, write};

use super::process;
use super::terminal_state::{RestoreGuard, child_output_restore_sequence, termination_requested};
use crate::recording::{Dwell, TraceBuilder};
use crate::trace::Trace;

/// Options for [`capture`].
#[derive(Debug, Clone)]
pub struct CaptureOpts {
    /// argv for the shell to spawn. Empty → use `$SHELL`, falling back
    /// to `bash`.
    pub argv: Vec<String>,
    /// Terminal columns. `None` → detect from the host tty (fallback `80`).
    pub cols: Option<u16>,
    /// Terminal rows. `None` → detect from the host tty (fallback `24`).
    pub rows: Option<u16>,
    /// Maximum wall-clock duration before the recorder force-stops.
    pub max_runtime: Duration,
}

/// Output event observed during live capture.
#[derive(Debug, Clone)]
pub struct CaptureEvent {
    /// Event timestamp in the trace timeline.
    pub time_s: f64,
    /// Output bytes read from the PTY.
    pub output: Vec<u8>,
    /// Dwell attached to this event in the recorder timeline.
    pub dwell_ms: u32,
}

/// Hook for consumers that want to process live capture output before
/// the full trace is finalized.
pub trait CaptureSink {
    /// Called after terminal geometry is resolved and before raw mode
    /// starts.
    ///
    /// # Errors
    /// Consumer-specific initialization failed.
    fn start(&mut self, cols: u16, rows: u16) -> Result<()>;

    /// Called for every non-empty PTY output event.
    ///
    /// # Errors
    /// Consumer-specific processing failed.
    fn output(&mut self, event: &CaptureEvent) -> Result<()>;
}

struct NullSink;

impl CaptureSink for NullSink {
    fn start(&mut self, _cols: u16, _rows: u16) -> Result<()> {
        Ok(())
    }

    fn output(&mut self, _event: &CaptureEvent) -> Result<()> {
        Ok(())
    }
}

impl Default for CaptureOpts {
    fn default() -> Self {
        Self {
            argv: Vec::new(),
            cols: None,
            rows: None,
            // 1h default; well above any sensible interactive recording.
            // `Duration::from_mins` is unstable; use seconds directly and
            // silence the larger-unit lint.
            #[allow(clippy::duration_suboptimal_units)]
            max_runtime: Duration::from_secs(3600),
        }
    }
}

/// Record a live terminal session and return the resulting trace.
///
/// Requires stdin to be a tty (`tcgetattr` fails otherwise). The
/// foreground loop exits when:
///
///   - the PTY hits EOF (child exited cleanly), or
///   - PTY read returns `EIO` (child closed the slave end), or
///   - stdin hits EOF, or
///   - `opts.max_runtime` elapses, or
///   - any IO error occurs.
///
/// On every exit path the host stdin's original termios is restored
/// via the `RawModeGuard` Drop impl.
///
/// # Errors
/// stdin is not a tty; PTY spawn failed; `tcsetattr` failed; an IO
/// error occurred during the loop other than the expected end-of-
/// session signals listed above.
///
/// # Panics
/// Never under normal use. The internal `100u16` timeout literal is
/// fed to a const conversion that cannot fail.
pub fn capture(opts: CaptureOpts) -> Result<Trace> {
    capture_with_sink(opts, &mut NullSink)
}

/// Record a live terminal session and feed each output event to `sink`
/// as it is captured.
///
/// This is the live-stitching entry point used by `ptyrecord`: capture
/// remains authoritative for the final trace, while the sink can render
/// frames or stream media concurrently with the user's session.
///
/// # Errors
/// Same as [`capture`], plus sink initialization or output errors.
pub fn capture_with_sink(opts: CaptureOpts, sink: &mut impl CaptureSink) -> Result<Trace> {
    let argv = resolve_argv(opts.argv);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (cols, rows) = resolve_geometry(opts.cols, opts.rows);
    sink.start(cols, rows)?;

    let mut pty = process::spawn(&argv_refs, cols, rows)?;
    let pty_fd = pty.fd();

    let stdin_fd = io::stdin().as_raw_fd();
    let stdout = io::stdout();
    let stdout_fd = stdout.as_raw_fd();
    let _terminal_cleanup = terminal_cleanup_guard(&stdout, stdout_fd);

    // RAII: original termios is restored when this drops, even on
    // error paths or panics. SIGKILL is the only way to leave the
    // host terminal stuck in raw mode.
    let _raw = RawModeGuard::enter(stdin_fd)?;

    let mut builder = TraceBuilder::new();
    let started = Instant::now();
    // Each step's dwell in the builder is the duration AFTER that step
    // before the next one (the "post-step interval"). On a PTY-master
    // read we don't yet know how long the just-read event will stay
    // before the next arrives, so we hold the read in `pending` and
    // flush it (recording its dwell against the next event's arrival
    // time) when the next event comes in. The final pending event is
    // flushed at loop exit with dwell = 0.
    //
    // This is the live-mode answer to a contract mismatch: the
    // TraceBuilder API treats `dwell` as "time the recorded data
    // remains on screen", but during live capture we only learn that
    // interval *retrospectively*, when the next event arrives. The
    // pending buffer is the off-by-one correction.
    let mut pending: Option<(Vec<u8>, Instant)> = None;
    // Cast timeline is accumulated in nanoseconds so a fast burst of
    // events doesn't get rounded to the same `time_s` when downstream
    // sinks compare adjacent events. `Instant::now()` is already
    // nanosecond-resolution on Linux/macOS; mirroring that precision
    // through the builder and out into the asciinema `time_s` field
    // costs nothing.
    let mut trace_time_ns = 0_u64;
    // 64 KiB buffer: PTY output bursts (e.g. `cargo build`, `ls -R`,
    // tmux redraws) can dump tens of kilobytes at once. A 4 KiB
    // buffer requires multiple read() syscalls and multiple cast
    // events to absorb a single kernel write — wasteful when humans
    // are watching. Heap-allocated (clippy refuses stack arrays
    // above 16 KiB and a one-time heap allocation per session is
    // negligible).
    let mut buf = vec![0u8; 65_536].into_boxed_slice();

    loop {
        if termination_requested() || started.elapsed() > opts.max_runtime {
            break;
        }

        let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
        let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        let mut fds = [
            PollFd::new(stdin_borrow, PollFlags::POLLIN),
            PollFd::new(pty_borrow, PollFlags::POLLIN),
        ];

        // 20 ms wakeup balances snappy termination response (max
        // 20 ms before Ctrl-C / max_runtime / SIGWINCH equivalents
        // are checked) against idle CPU when nobody is typing.
        // poll is level-triggered, so live throughput is never
        // bounded by this timeout — only idle wakeup cadence is.
        let timeout = PollTimeout::from(20u16);
        match poll(&mut fds, timeout) {
            // Timeout (Ok(0)) and EINTR are both "no events; loop again";
            // any other error halts.
            Err(Errno::EINTR) if termination_requested() => break,
            Ok(0) | Err(Errno::EINTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(anyhow!("poll: {e}")),
        }

        // stdin → PTY (forward user keystrokes).
        if let Some(rev) = fds[0].revents()
            && rev.intersects(PollFlags::POLLIN)
        {
            let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
            match read(stdin_borrow, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    write_all(pty_fd, &buf[..n]).context("write stdin -> pty")?;
                }
                Err(Errno::EINTR) if termination_requested() => break,
                Err(Errno::EINTR) => {}
                Err(e) => return Err(anyhow!("read stdin: {e}")),
            }
        }

        // PTY → stdout + record (visibility + capture).
        if let Some(rev) = fds[1].revents() {
            if rev.intersects(PollFlags::POLLIN) {
                let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
                match read(pty_borrow, &mut buf) {
                    // EOF (Ok(0)) and EIO both mean the slave end closed —
                    // child exited normally; the recorder should finalize
                    // rather than report an error.
                    Ok(0) | Err(Errno::EIO) => break,
                    Ok(n) => {
                        let bytes = &buf[..n];
                        // Best-effort tee: a stalled stdout shouldn't
                        // halt the recording. Worst case the user
                        // misses a frame; the trace still has it.
                        let _ = write_all(stdout_fd, bytes);
                        let now = Instant::now();
                        // Flush the previous pending event with the
                        // dwell it actually stayed on screen for —
                        // measured retrospectively as (now - prev_arrival).
                        if let Some((prev_bytes, prev_time)) = pending.take() {
                            flush_pending(
                                &mut builder,
                                sink,
                                &mut trace_time_ns,
                                prev_bytes,
                                now.saturating_duration_since(prev_time),
                            )?;
                        }
                        pending = Some((bytes.to_vec(), now));
                    }
                    Err(Errno::EINTR) if termination_requested() => break,
                    Err(Errno::EINTR) => {}
                    Err(e) => return Err(anyhow!("read pty: {e}")),
                }
            }
            if rev.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
                break;
            }
        }
    }

    // Flush the final pending event with dwell 0 — no more events will
    // arrive, so there's no "interval before the next one" to record.
    // Asciinema players hold the final frame indefinitely.
    if let Some((bytes, _)) = pending.take() {
        flush_pending(
            &mut builder,
            sink,
            &mut trace_time_ns,
            bytes,
            Duration::ZERO,
        )?;
    }

    pty.terminate_child();
    let recording = builder.finish_screen(cols, rows)?;
    Ok(recording.into_trace())
}

/// Flush one buffered PTY-output event into the trace builder and live
/// sink, assigning the dwell that was measured against the next event's
/// arrival (or 0 if this is the final event). Updates `trace_time_ns`
/// so the next event's `CaptureEvent.time_s` reflects cumulative dwell.
///
/// The `bytes` Vec is consumed by the builder; the sink sees a
/// reference and must not retain the buffer past the call.
fn flush_pending(
    builder: &mut TraceBuilder,
    sink: &mut impl CaptureSink,
    trace_time_ns: &mut u64,
    bytes: Vec<u8>,
    elapsed: Duration,
) -> Result<()> {
    let dwell = Dwell::from_duration(elapsed);
    let time_s = ns_to_seconds(*trace_time_ns);
    // Sink consumers (frame rendering, ffmpeg piping) work in
    // milliseconds — sub-ms precision wouldn't survive the video
    // pipeline anyway. Internally the builder keeps the full ns
    // dwell so cast `time_s` retains its precision.
    let dwell_ms = dwell.as_millis_u32();
    // Build the event in place — the sink only borrows it, so we
    // avoid a redundant clone of the bytes that the builder then
    // moves. Allocate the Vec once at read time, hand the same
    // allocation to the builder.
    let event = CaptureEvent {
        time_s,
        output: bytes,
        dwell_ms,
    };
    sink.output(&event).context("capture sink output")?;
    builder
        .record_output(event.output, dwell)
        .context("record_output")?;
    *trace_time_ns = trace_time_ns.saturating_add(dwell.as_nanos());
    Ok(())
}

fn write_all(fd: RawFd, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("live capture write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("live capture write failed: {err}")),
        }
    }
    Ok(())
}

fn ns_to_seconds(ns: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = ns as f64;
    n / 1_000_000_000.0
}

fn resolve_argv(argv: Vec<String>) -> Vec<String> {
    if !argv.is_empty() {
        return argv;
    }
    if let Ok(sh) = std::env::var("SHELL")
        && !sh.is_empty()
    {
        return vec![sh];
    }
    vec!["bash".into()]
}

fn resolve_geometry(cols: Option<u16>, rows: Option<u16>) -> (u16, u16) {
    let (auto_c, auto_r) = detect_tty_size().unwrap_or((80, 24));
    (cols.unwrap_or(auto_c), rows.unwrap_or(auto_r))
}

fn terminal_cleanup_guard(stdout: &io::Stdout, fd: RawFd) -> Option<RestoreGuard> {
    if cfg!(test) {
        return None;
    }
    stdout
        .is_terminal()
        .then_some(RestoreGuard::new(fd, child_output_restore_sequence()))
}

fn detect_tty_size() -> Option<(u16, u16)> {
    use nix::libc;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `ws` is a valid &mut winsize for the duration of the
    // call; STDOUT_FILENO is always a valid fd in a hosted process
    // (closed-stdio is undefined behavior territory we don't enter).
    let ret = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut ws) };
    if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

/// RAII guard that puts a tty fd into raw mode on construction and
/// restores the original termios on drop.
struct RawModeGuard {
    fd: RawFd,
    original: Termios,
}

impl RawModeGuard {
    fn enter(fd: RawFd) -> Result<Self> {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let original =
            tcgetattr(borrowed).context("tcgetattr — is stdin a tty? (live mode requires one)")?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        tcsetattr(borrowed, SetArg::TCSAFLUSH, &raw).context("tcsetattr to raw")?;
        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = tcsetattr(borrowed, SetArg::TCSAFLUSH, &self.original);
    }
}
