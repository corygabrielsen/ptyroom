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

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{read, write};

use super::process;
use crate::recording::{DwellMs, TraceBuilder};
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
    let stdout_fd = io::stdout().as_raw_fd();

    // RAII: original termios is restored when this drops, even on
    // error paths or panics. SIGKILL is the only way to leave the
    // host terminal stuck in raw mode.
    let _raw = RawModeGuard::enter(stdin_fd)?;

    let mut builder = TraceBuilder::new();
    let started = Instant::now();
    let mut last_event = started;
    let mut trace_time_ms = 0_u64;
    let mut buf = [0u8; 4096];

    loop {
        if started.elapsed() > opts.max_runtime {
            break;
        }

        let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
        let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        let mut fds = [
            PollFd::new(stdin_borrow, PollFlags::POLLIN),
            PollFd::new(pty_borrow, PollFlags::POLLIN),
        ];

        // 100 ms wakeup so the max_runtime check fires within a beat
        // even when nobody types.
        let timeout = PollTimeout::from(100u16);
        match poll(&mut fds, timeout) {
            // Timeout (Ok(0)) and EINTR are both "no events; loop again";
            // any other error halts.
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
                    let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
                    write(pty_borrow, &buf[..n]).context("write stdin → pty")?;
                }
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
                        let stdout_borrow = unsafe { BorrowedFd::borrow_raw(stdout_fd) };
                        // Best-effort tee: a stalled stdout shouldn't
                        // halt the recording. Worst case the user
                        // misses a frame; the trace still has it.
                        let _ = write(stdout_borrow, bytes);
                        let now = Instant::now();
                        let dwell =
                            DwellMs::from_duration(now.saturating_duration_since(last_event));
                        let event = CaptureEvent {
                            time_s: ms_to_seconds(trace_time_ms),
                            output: bytes.to_vec(),
                            dwell_ms: dwell.get(),
                        };
                        sink.output(&event).context("capture sink output")?;
                        builder
                            .record_output(bytes.to_vec(), dwell)
                            .context("record_output")?;
                        trace_time_ms = trace_time_ms.saturating_add(u64::from(dwell.get()));
                        last_event = now;
                    }
                    Err(Errno::EINTR) => {}
                    Err(e) => return Err(anyhow!("read pty: {e}")),
                }
            }
            if rev.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
                break;
            }
        }
    }

    pty.terminate_child();
    let recording = builder.finish_screen(cols, rows)?;
    Ok(recording.into_trace())
}

fn ms_to_seconds(ms: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = ms as f64;
    n / 1000.0
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
