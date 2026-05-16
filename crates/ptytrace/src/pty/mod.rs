//! PTY recording and shared-terminal transport.
//!
//! Local recording paths spawn an interactive terminal process via PTY,
//! drive it by sending scripted keystrokes/dwells, capture every byte
//! written, and emit an asciinema v2-compatible trace with deterministic
//! timestamps.
//!
//! Collaborative paths reuse the same PTY mechanics: [`share`] hosts one
//! PTY, [`connect`] attaches another terminal to it, and `ptyroom` wraps
//! both as the high-level CLI.
//!
//! The trace's per-event timestamp is the cumulative sum of the *intended*
//! dwell, not wall-clock — playback is independent of the speed of the
//! recording machine.

pub mod connect;
pub mod share;

mod drainer;
mod input_router;
mod keys;
mod live;
mod osc;
mod process;
mod room_protocol;
mod status_bar;
mod terminal_io;
mod terminal_state;
mod viewport;

pub use drainer::WatchHandle;
pub use keys::Key;
pub use live::{CaptureEvent, CaptureOpts, CaptureSink, capture, capture_with_sink};
pub use osc::StubColors;

use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use nix::unistd::write;
use tempfile::NamedTempFile;

use crate::recording::{Dwell, TraceBuilder};
use crate::trace::Trace;
use drainer::Drainer;
use process::{PtyMaster, spawn_with_env as spawn_pty};

/// Short fallback wall-clock window for inputs that do not have a stronger
/// content-aware sync point. Text entry and shell commands use watches;
/// this mainly covers raw keys like picker arrows and heredoc newlines.
const DEFAULT_SETTLE: Duration = Duration::from_micros(5);
const ECHO_TIMEOUT: Duration = Duration::from_millis(250);
static CONTAINER_HOME_SEQ: AtomicU64 = AtomicU64::new(0);

/// Bash startup profile used by the Docker convenience launcher.
///
/// The recorder core can spawn any argv via [`PtyTracer::spawn`]. This profile
/// is only for [`PtyTracer::start`], which starts bash in the configured
/// container/image and needs a reproducible prompt and initial screen state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    /// Raw bash lines executed before the prompt is installed.
    ///
    /// These are intentionally shell lines, not a typed DSL. The profile owns
    /// shell startup policy; the recorder owns PTY IO and timing.
    pub setup_commands: Vec<String>,
    /// Bash `PS1` value.
    pub prompt: String,
    /// Whether to clear the terminal after startup setup.
    pub clear_on_start: bool,
    /// Optional verbatim rcfile bytes. When `Some`, the structured
    /// fields above are ignored and these bytes are written to the
    /// rcfile as-is. Used by the script DSL to pass through a heredoc
    /// `SetShellRcfile` block.
    pub raw_rcfile: Option<Vec<u8>>,
}

/// Bytes inserted into the presentation stream without touching the PTY.
///
/// Presentation output is visible in the final trace, but it is marked
/// separately from child-process output in the raw evidence log. Use it for
/// visual structure such as labels or blank prompt lines, not for state that
/// the child process must observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationOutput {
    bytes: Vec<u8>,
}

impl PresentationOutput {
    #[must_use]
    pub fn bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            bytes: text.into().into_bytes(),
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl From<Vec<u8>> for PresentationOutput {
    fn from(bytes: Vec<u8>) -> Self {
        Self::bytes(bytes)
    }
}

impl From<&[u8]> for PresentationOutput {
    fn from(bytes: &[u8]) -> Self {
        Self::bytes(bytes.to_vec())
    }
}

impl From<String> for PresentationOutput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for PresentationOutput {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

impl ShellProfile {
    #[must_use]
    pub fn simple() -> Self {
        Self {
            setup_commands: vec!["cd \"$HOME\"".into()],
            prompt: "$ ".into(),
            clear_on_start: true,
            raw_rcfile: None,
        }
    }
}

impl Default for ShellProfile {
    fn default() -> Self {
        Self::simple()
    }
}

#[derive(Debug, Clone)]
pub struct PtyTracerConfig {
    /// Terminal columns.
    pub cols: u16,
    /// Terminal rows.
    pub rows: u16,
    /// Docker image used when no warm container is configured.
    pub image: String,
    /// Optional running container name/id for warm `docker exec` recording.
    pub container: Option<String>,
    /// Command executed inside a warm container.
    ///
    /// Warm containers cannot receive a host-mounted rcfile per recording, so
    /// this command is responsible for applying the desired shell profile.
    pub warm_command: Vec<String>,
    /// Parent directory under which each warm-container recording gets a
    /// fresh `$HOME` (`<warm_home_root>/.ptytrace-home-<pid>-<seq>`).
    /// The wrapper named in [`PtyTracerConfig::warm_command`] is responsible
    /// for `mkdir`-ing this path inside the container before exec-ing the
    /// shell. Default is `/tmp` (universally writable on POSIX); override
    /// to point at any path the in-container user can `mkdir` under.
    pub warm_home_root: PathBuf,
    /// Bash startup profile for cold `docker run` recordings.
    pub shell: ShellProfile,
    /// Extra environment variables for the spawned child process.
    ///
    /// Values override inherited variables with the same name. For Docker
    /// targets these are passed as `-e KEY=value`; for local spawn targets they
    /// are passed directly through the PTY process builder.
    pub env: Vec<(String, String)>,
    /// Stubbed terminal color query responses.
    pub stubs: StubColors,
    /// Wall-clock guard against hung child processes.
    pub max_runtime: Duration,
}

impl Default for PtyTracerConfig {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 30,
            image: "bash:latest".into(),
            container: std::env::var("PTYTRACE_CONTAINER")
                .ok()
                .filter(|value| !value.is_empty()),
            warm_command: vec!["bash".into(), "-i".into()],
            warm_home_root: PathBuf::from("/tmp"),
            shell: ShellProfile::simple(),
            env: Vec::new(),
            stubs: StubColors::default(),
            max_runtime: Duration::from_mins(4),
        }
    }
}

pub struct PtyTracer {
    cfg: PtyTracerConfig,
    pty: PtyMaster,
    drainer: Drainer,
    /// Hold the cold `docker run` rcfile alive — `Drop` unlinks it.
    _rcfile: Option<NamedTempFile>,
    recording: TraceBuilder,
    started_at: Instant,
}

impl PtyTracer {
    /// Spawn an arbitrary interactive process under a PTY.
    ///
    /// This is the reusable-library entry point: callers own the child process,
    /// environment, shell profile, and any domain-specific setup. The recorder
    /// owns PTY IO, terminal geometry, OSC stubbing, and deterministic trace
    /// timestamps.
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use ptytrace::pty::{PtyTracer, PtyTracerConfig};
    ///
    /// let mut rec = PtyTracer::spawn(PtyTracerConfig::default(), &["bash"])?;
    /// rec.send_raw_wait_for(
    ///     &[], Duration::ZERO,
    ///     b"$ ", Duration::from_secs(2),
    ///     "prompt",
    /// )?;
    /// rec.type_text("echo hello", Duration::from_millis(35))?;
    /// rec.stop()?.write("hello.ptytrace")?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    ///
    /// # Errors
    /// Empty argv, or `forkpty` / `execvp(argv[0])` failure.
    pub fn spawn(cfg: PtyTracerConfig, argv: &[&str]) -> anyhow::Result<Self> {
        let pty = spawn_pty(argv, &cfg.env, cfg.cols, cfg.rows).context("spawn process")?;
        Ok(Self::from_pty(cfg, pty, None))
    }

    /// Spawn or exec interactive bash inside the recording container.
    ///
    /// With `cfg.container = None`, this runs `docker run --rm ...`.
    /// With `cfg.container = Some(name)`, this uses `docker exec` against an
    /// already-running warm container and creates a fresh `$HOME` for the
    /// recording.
    ///
    /// The docker-side TTY size is set via `-e LINES/COLUMNS` and inherited
    /// from the host PTY's winsize (TIOCSWINSZ via `forkpty`).
    ///
    /// # Errors
    /// rcfile write fails, or `forkpty` / `execvp("docker")` fails.
    pub fn start(cfg: PtyTracerConfig) -> anyhow::Result<Self> {
        if cfg.container.is_some() && cfg.warm_command.is_empty() {
            anyhow::bail!("warm recorder requires at least one warm_command arg");
        }
        let rcfile = if cfg.container.is_some() {
            None
        } else {
            Some(build_rcfile(&cfg.shell).context("write rcfile")?)
        };
        let argv = if let Some(container) = &cfg.container {
            warm_docker_argv(&cfg, container)
        } else {
            let Some(rcfile_handle) = &rcfile else {
                anyhow::bail!("cold recorder missing rcfile");
            };
            cold_docker_argv(&cfg, rcfile_handle.path())
        };
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let pty = spawn_pty(&argv_refs, &[], cfg.cols, cfg.rows).context("spawn docker run")?;
        Ok(Self::from_pty(cfg, pty, rcfile))
    }

    fn from_pty(cfg: PtyTracerConfig, pty: PtyMaster, rcfile: Option<NamedTempFile>) -> Self {
        let drainer = Drainer::start(pty.fd(), cfg.stubs);
        Self {
            cfg,
            pty,
            drainer,
            _rcfile: rcfile,
            recording: TraceBuilder::new(),
            started_at: Instant::now(),
        }
    }

    #[must_use]
    pub fn event_count(&self) -> usize {
        self.recording.event_count()
    }
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.cfg.cols
    }
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.cfg.rows
    }

    /// Dwell at the current state for `dwell` of playback time, with `settle`
    /// of real wall-clock time to let the container produce output.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`, or PTY write/read failed.
    pub fn dwell(&mut self, dwell: Duration, settle: Duration) -> anyhow::Result<()> {
        self.send_and_capture(&[], dwell, settle)
    }

    /// Send a single key, dwell `dwell` after.
    ///
    /// # Errors
    /// As [`PtyTracer::dwell`].
    pub fn key(&mut self, key: Key, dwell: Duration) -> anyhow::Result<()> {
        self.send_and_capture(key.bytes(), dwell, DEFAULT_SETTLE)
    }

    /// Send a single key with an explicit capture settle.
    ///
    /// Use this for interactive programs where each key should become a
    /// distinct visible frame. Content-aware waits are still preferred when
    /// the program emits a reliable marker.
    ///
    /// # Errors
    /// As [`PtyTracer::dwell`].
    pub fn key_settle(
        &mut self,
        key: Key,
        dwell: Duration,
        settle: Duration,
    ) -> anyhow::Result<()> {
        self.send_and_capture(key.bytes(), dwell, settle)
    }

    /// Send a key `repeat` times, with `dwell` between each.
    ///
    /// # Errors
    /// As [`PtyTracer::dwell`].
    pub fn keys(&mut self, key: Key, dwell: Duration, repeat: usize) -> anyhow::Result<()> {
        for _ in 0..repeat {
            self.key(key, dwell)?;
        }
        Ok(())
    }

    /// Send repeated raw key bytes as one live burst while advancing trace time
    /// as if each key happened at `dwell` cadence.
    ///
    /// This is the virtual-time form of [`PtyTracer::keys`]: useful when the
    /// child program can process queued input deterministically and the demo
    /// does not need host sleeps between individual keys.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`, or PTY write failed.
    pub fn keys_burst(&mut self, key: Key, dwell: Duration, repeat: usize) -> anyhow::Result<()> {
        if repeat == 0 {
            return Ok(());
        }

        self.check_runtime()?;
        let bytes = key.bytes();
        let mut input = Vec::with_capacity(bytes.len() * repeat);
        for _ in 0..repeat {
            input.extend_from_slice(bytes);
        }
        write_all(self.pty.fd(), &input)?;

        let total_dwell = dwell.saturating_mul(u32::try_from(repeat).unwrap_or(u32::MAX));
        let captured = self.drainer.consume();
        self.recording
            .record_step(input, captured, Dwell::from_duration(total_dwell))?;
        Ok(())
    }

    /// Add presentation output directly to the trace without touching the PTY.
    ///
    /// This is the low-level presentation-time escape hatch. The output is
    /// visible in the trace and explicitly marked as presentation output in
    /// the raw evidence log.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`.
    pub fn push_presentation_output(
        &mut self,
        output: impl Into<PresentationOutput>,
        dwell: Duration,
    ) -> anyhow::Result<()> {
        self.check_runtime()?;
        self.recording
            .record_presentation_output(output.into().into_bytes(), Dwell::from_duration(dwell))?;
        Ok(())
    }

    /// Advance trace presentation time without waiting on the child process.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`.
    pub fn advance_virtual_time(&mut self, dwell: Duration) -> anyhow::Result<()> {
        self.push_presentation_output(Vec::new(), dwell)
    }

    /// Type text in the trace without sending it to the shell.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`.
    pub fn type_presentation_text(&mut self, text: &str, per_char: Duration) -> anyhow::Result<()> {
        self.check_runtime()?;
        for ch in text.chars() {
            let mut buf = [0_u8; 4];
            self.recording.record_presentation_output(
                ch.encode_utf8(&mut buf).as_bytes().to_vec(),
                Dwell::from_duration(per_char),
            )?;
        }
        Ok(())
    }

    /// Type a UTF-8 string, one codepoint at a time, dwelling `per_char`
    /// after each byte sequence.
    ///
    /// # Errors
    /// As [`PtyTracer::dwell`].
    pub fn type_text(&mut self, text: &str, per_char: Duration) -> anyhow::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        self.check_runtime()?;
        let pending = self.drainer.consume();
        if !pending.is_empty() {
            self.recording
                .record_output(pending, Dwell::from_duration(Duration::ZERO))?;
        }

        let expected = text.as_bytes();
        let watch = self.arm_watch(expected);
        write_all(self.pty.fd(), expected)?;
        watch.wait(ECHO_TIMEOUT).ok_or_else(|| {
            anyhow::anyhow!(
                "typed span echo timed out after {}ms waiting for {}",
                ECHO_TIMEOUT.as_millis(),
                escape_bytes(expected),
            )
        })?;

        let captured = self.drainer.consume();
        let Some(echo_start) = find_subslice(&captured, expected) else {
            anyhow::bail!(
                "typed span echo fired but captured output did not contain {}",
                escape_bytes(expected),
            );
        };

        if echo_start > 0 {
            self.recording.record_output(
                captured[..echo_start].to_vec(),
                Dwell::from_duration(Duration::ZERO),
            )?;
        }

        let mut is_first_char = true;
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            let bytes = ch.encode_utf8(&mut buf).as_bytes();
            if is_first_char {
                self.recording.record_step(
                    expected.to_vec(),
                    bytes.to_vec(),
                    Dwell::from_duration(per_char),
                )?;
                is_first_char = false;
            } else {
                self.recording
                    .record_output(bytes.to_vec(), Dwell::from_duration(per_char))?;
            }
        }

        let echo_end = echo_start + expected.len();
        if echo_end < captured.len() {
            self.recording.record_output(
                captured[echo_end..].to_vec(),
                Dwell::from_duration(Duration::ZERO),
            )?;
        }
        Ok(())
    }

    /// Send raw bytes (escape sequences, control codes) with the given dwell.
    ///
    /// # Errors
    /// As [`PtyTracer::dwell`].
    pub fn send_raw(&mut self, bytes: &[u8], dwell: Duration) -> anyhow::Result<()> {
        self.send_and_capture(bytes, dwell, DEFAULT_SETTLE)
    }

    /// Send raw bytes and record output after `pattern` appears in the PTY
    /// stream. The wait is wall-clock synchronization only; playback time
    /// still advances by `dwell`.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`, PTY write failed, or `pattern`
    /// was not observed before `timeout`.
    pub fn send_raw_wait_for(
        &mut self,
        bytes: &[u8],
        dwell: Duration,
        pattern: &[u8],
        timeout: Duration,
        label: &str,
    ) -> anyhow::Result<Duration> {
        self.send_and_capture_wait_for(bytes, dwell, pattern, timeout, label)
    }

    /// Arm a content-aware sync point: the drainer starts watching the
    /// PTY output stream for `pattern` from this moment forward and
    /// returns a [`WatchHandle`]. Block on `WatchHandle::wait` to sleep
    /// until the pattern is observed (or a timeout elapses).
    ///
    /// **Arm before triggering.** Call this *before* the action that
    /// causes the pattern to be emitted. Otherwise the bytes can arrive
    /// during the action's settle window and be consumed before the
    /// watch is in place — the watch then never fires.
    ///
    /// Trace time is **not** advanced by waiting; bytes that arrive
    /// during the wait are folded into the next `dwell`/`key` event.
    /// Combine with a small explicit `dwell` for trace-side visible
    /// time:
    ///
    /// ```ignore
    /// let alt_in = r.arm_watch(b"\x1b[?1049h");
    /// r.type_text("vim", per_char)?;
    /// r.key(Key::Enter, ms(0))?;
    /// alt_in.wait(STARTUP_TIMEOUT).expect("alt-screen entry");
    /// r.dwell(STARTUP_VISIBLE, ms(0))?;   // small trace-time buffer
    /// ```
    ///
    /// When `PTYTRACE_PROFILE=1` is set in the environment,
    /// `WatchHandle::wait` logs the pattern + elapsed time to stderr.
    /// Use this to tune timeouts: run the demo once with the env var,
    /// observe actual wait times, then bump the timeout constants down
    /// to ~2-3× observed.
    #[must_use]
    pub fn arm_watch(&self, pattern: &[u8]) -> WatchHandle {
        self.drainer.register_watch(pattern.to_vec())
    }

    /// Write bytes without creating a recorded playback event.
    ///
    /// This is for trace/timeline capture paths where presentation time
    /// is compiled later instead of being attached to each write.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`, or PTY write failed.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.check_runtime()?;
        if !bytes.is_empty() {
            write_all(self.pty.fd(), bytes)?;
        }
        Ok(())
    }

    /// Capture bytes after a real-time settle. Records observed bytes
    /// into the trace at zero playback dwell (so wait-style polls don't
    /// inflate the trace's virtual time), and returns them for caller
    /// inspection (e.g. regex matching).
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`.
    pub fn capture_after(&mut self, settle: Duration) -> anyhow::Result<Vec<u8>> {
        self.check_runtime()?;
        std::thread::sleep(settle);
        let captured = self.drainer.consume();
        if !captured.is_empty() {
            self.recording
                .record_output(captured.clone(), Dwell::from_duration(Duration::ZERO))?;
        }
        Ok(captured)
    }

    /// Block until `pattern` matches in PTY output. Captured bytes up
    /// to and including the pattern's match-end become a single trace
    /// event with the given `dwell`. Trailing bytes (after the pattern)
    /// are pushed back to the drainer for the next operation.
    ///
    /// This is the regex parallel to the literal-pattern flow used
    /// internally by [`Self::send_raw_wait_for`]. The same `pattern_end`
    /// cutoff matters for partition determinism: without it, a slow
    /// poll wake can scoop up post-pattern bytes that on a fast wake
    /// would belong to the next event, manifesting as event-count
    /// drift downstream.
    ///
    /// # Errors
    /// `timeout` elapsed without the pattern matching, or
    /// `max_runtime` exceeded.
    pub fn wait_for_regex(
        &mut self,
        pattern: &regex::bytes::Regex,
        dwell: Duration,
        timeout: Duration,
        label: &str,
    ) -> anyhow::Result<()> {
        let started = Instant::now();
        let poll_interval = Duration::from_millis(20);
        let mut accumulated: Vec<u8> = Vec::new();
        loop {
            self.check_runtime()?;
            std::thread::sleep(poll_interval);
            accumulated.extend_from_slice(&self.drainer.consume());
            if let Some(m) = pattern.find(&accumulated) {
                let pattern_end = m.end();
                let (this_event, leftover) = accumulated.split_at(pattern_end);
                let this_event = this_event.to_vec();
                if !leftover.is_empty() {
                    self.drainer.unconsume(leftover.to_vec());
                }
                if this_event.is_empty() && dwell.is_zero() {
                    return Ok(());
                }
                self.recording
                    .record_output(this_event, Dwell::from_duration(dwell))?;
                return Ok(());
            }
            if started.elapsed() >= timeout {
                anyhow::bail!(
                    "wait_for_regex /{}/ ({label}) timed out after {}ms",
                    pattern.as_str(),
                    timeout.as_millis(),
                );
            }
        }
    }

    fn send_and_capture(
        &mut self,
        bytes: &[u8],
        dwell: Duration,
        settle: Duration,
    ) -> anyhow::Result<()> {
        self.check_runtime()?;
        if !bytes.is_empty() {
            write_all(self.pty.fd(), bytes)?;
        }
        std::thread::sleep(settle);
        let captured = self.drainer.consume();
        self.recording
            .record_step(bytes.to_vec(), captured, Dwell::from_duration(dwell))?;
        Ok(())
    }

    fn send_and_capture_wait_for(
        &mut self,
        bytes: &[u8],
        dwell: Duration,
        pattern: &[u8],
        timeout: Duration,
        label: &str,
    ) -> anyhow::Result<Duration> {
        self.check_runtime()?;
        let watch = self.arm_watch(pattern);
        if !bytes.is_empty() {
            write_all(self.pty.fd(), bytes)?;
        }
        let elapsed = watch.wait(timeout).ok_or_else(|| {
            anyhow::anyhow!(
                "{label} timed out after {}ms waiting for {}",
                timeout.as_millis(),
                escape_bytes(pattern),
            )
        })?;
        let captured = self.drainer.consume();
        // Tight cutoff: this event contains bytes up to and including
        // the pattern. Anything after stays in the drainer buffer for
        // the next operation. Without this split, a slow recorder-thread
        // wake under contention can scoop up post-pattern bytes that on
        // a faster wake would belong to the next event — a partition
        // race that surfaces as event-count drift in downstream
        // artifacts.
        let pattern_end =
            find_subslice(&captured, pattern).map_or(captured.len(), |i| i + pattern.len());
        let (this_event, leftover) = captured.split_at(pattern_end);
        let this_event = this_event.to_vec();
        if !leftover.is_empty() {
            self.drainer.unconsume(leftover.to_vec());
        }
        self.recording
            .record_step(bytes.to_vec(), this_event, Dwell::from_duration(dwell))?;
        Ok(elapsed)
    }

    fn check_runtime(&self) -> anyhow::Result<()> {
        if self.started_at.elapsed() > self.cfg.max_runtime {
            anyhow::bail!(
                "recording exceeded max_runtime={}ms (child hung or script too long?)",
                self.cfg.max_runtime.as_millis(),
            );
        }
        Ok(())
    }

    /// Terminate the child and stop the drainer. The recorder is consumed —
    /// call [`Trace::write`] on the returned trace afterwards.
    ///
    /// # Errors
    /// `finish_synthetic` failed to assemble the recorded trace.
    pub fn stop(mut self) -> anyhow::Result<Trace> {
        let mut trace = self
            .recording
            .finish_synthetic(self.cfg.cols, self.cfg.rows)?
            .into_trace();
        for (key, value) in &self.cfg.env {
            trace.header.env.insert(key.clone(), value.clone());
        }
        self.pty.terminate_child();
        // Drop fields in order: drainer joins, _rcfile unlinks.
        Ok(trace)
    }
}

fn write_all(fd: i32, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        // Safety: caller holds an OwnedFd whose lifetime exceeds this call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("pty write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(nix::errno::Errno::EINTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

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

/// Write the rcfile bash will source at startup.
///
/// # Errors
/// IO error creating or writing the temp file.
fn build_rcfile(profile: &ShellProfile) -> std::io::Result<NamedTempFile> {
    use std::io::Write;
    let mut f = tempfile::Builder::new()
        .prefix("ptytrace-rc-")
        .suffix(".rc")
        .tempfile()?;
    if let Some(raw) = &profile.raw_rcfile {
        f.write_all(raw)?;
    } else {
        for line in &profile.setup_commands {
            writeln!(f, "{line}")?;
        }
        writeln!(f, "PS1={}", shell_single_quote(&profile.prompt))?;
        if profile.clear_on_start {
            writeln!(f, "printf '\\033[H\\033[2J\\033[3J'")?;
        }
    }
    f.flush()?;
    Ok(f)
}

fn shell_single_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let mut quoted = String::with_capacity(s.len() + 2);
    quoted.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn append_docker_env(argv: &mut Vec<String>, env: &[(String, String)]) {
    for (key, value) in env {
        argv.push("-e".into());
        argv.push(format!("{key}={value}"));
    }
}

/// Build the `docker exec ... <container> <warm_command>` argv for
/// reusing an already-running warm container. Allocates a fresh
/// `$HOME` under `warm_home_root` so each recording sees a clean
/// shell history / dotfile state.
fn warm_docker_argv(cfg: &PtyTracerConfig, container: &str) -> Vec<String> {
    let seq = CONTAINER_HOME_SEQ.fetch_add(1, Ordering::Relaxed);
    let home = cfg
        .warm_home_root
        .join(format!(".ptytrace-home-{}-{seq}", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let mut argv = vec![
        "docker".into(),
        "exec".into(),
        "-i".into(),
        "-t".into(),
        "-e".into(),
        format!("LINES={}", cfg.rows),
        "-e".into(),
        format!("COLUMNS={}", cfg.cols),
        "-e".into(),
        format!("HOME={home}"),
    ];
    append_docker_env(&mut argv, &cfg.env);
    argv.push(container.to_string());
    argv.extend(cfg.warm_command.iter().cloned());
    argv
}

/// Build the `docker run --rm ... bash --rcfile ...` argv for a cold
/// recording. Mounts the caller-prepared rcfile into the container
/// at `/tmp/recorderrc` and starts an interactive bash that sources
/// it.
fn cold_docker_argv(cfg: &PtyTracerConfig, rcfile_path: &Path) -> Vec<String> {
    let mount = format!("{}:/tmp/recorderrc:ro", rcfile_path.display());
    let mut argv = vec![
        "docker".into(),
        "run".into(),
        "--rm".into(),
        "-i".into(),
        "-t".into(),
        "-e".into(),
        format!("LINES={}", cfg.rows),
        "-e".into(),
        format!("COLUMNS={}", cfg.cols),
    ];
    append_docker_env(&mut argv, &cfg.env);
    argv.extend([
        "-v".into(),
        mount,
        cfg.image.clone(),
        "bash".into(),
        "--rcfile".into(),
        "/tmp/recorderrc".into(),
        "-i".into(),
    ]);
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_generic() {
        let cfg = PtyTracerConfig::default();
        assert_eq!(cfg.cols, 80);
        assert_eq!(cfg.rows, 30);
        assert_eq!(cfg.image, "bash:latest");
        assert_eq!(cfg.warm_command, ["bash", "-i"]);
        assert_eq!(cfg.shell, ShellProfile::simple());
        assert!(cfg.env.is_empty());
    }

    #[test]
    fn rcfile_contains_cd_and_ps1_and_clear() {
        let f = build_rcfile(&ShellProfile::simple()).unwrap();
        let s = std::fs::read_to_string(f.path()).unwrap();
        assert!(s.contains("cd \"$HOME\""));
        assert!(s.contains("PS1="));
        assert!(s.contains("printf"));
    }

    #[test]
    fn rcfile_uses_caller_supplied_shell_profile() {
        let f = build_rcfile(&ShellProfile {
            setup_commands: vec!["cd /work".into(), "export DEMO=1".into()],
            prompt: "demo's $ ".into(),
            clear_on_start: false,
            raw_rcfile: None,
        })
        .unwrap();
        let s = std::fs::read_to_string(f.path()).unwrap();
        assert!(s.contains("cd /work\n"));
        assert!(s.contains("export DEMO=1\n"));
        assert!(s.contains("PS1='demo'\\''s $ '\n"));
        assert!(!s.contains("\\033[H"));
    }

    #[test]
    fn shell_quote_handles_empty_and_plain_values() {
        assert_eq!(shell_single_quote(""), "''");
        assert_eq!(shell_single_quote("$ "), "'$ '");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn docker_env_is_encoded_as_explicit_e_flags() {
        let mut argv = vec!["docker".to_owned(), "run".to_owned()];

        append_docker_env(
            &mut argv,
            &[
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("PS1".to_owned(), "$ ".to_owned()),
            ],
        );

        assert_eq!(
            argv,
            ["docker", "run", "-e", "TERM=xterm-256color", "-e", "PS1=$ "]
        );
    }
}
