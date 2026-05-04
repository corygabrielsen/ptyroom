//! Recorder library.
//!
//! Spawns an interactive terminal process via PTY, drives it by sending
//! scripted keystrokes/dwells, captures every byte written, and emits an
//! asciinema v2 cast with deterministic timestamps.
//!
//! The cast's per-event timestamp is the cumulative sum of the *intended*
//! dwell, not wall-clock — playback is independent of the speed of the
//! recording machine.

mod drainer;
mod keys;
mod osc;
mod pty;

pub use drainer::WatchHandle;
pub use keys::Key;
pub use osc::StubColors;

use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use nix::unistd::write;
use tempfile::NamedTempFile;

use crate::cast::Cast;
use crate::proof::DwellMs;
use crate::recording::RecordingBuilder;
use drainer::Drainer;
use pty::{PtyMaster, spawn as spawn_pty};

/// Short fallback wall-clock window for inputs that do not have a stronger
/// content-aware sync point. Text entry and shell commands use watches;
/// this mainly covers raw keys like picker arrows and heredoc newlines.
const DEFAULT_SETTLE: Duration = Duration::from_micros(5);
const ECHO_TIMEOUT: Duration = Duration::from_millis(250);
static CONTAINER_HOME_SEQ: AtomicU64 = AtomicU64::new(0);

/// Bash startup profile used by the Docker convenience launcher.
///
/// The recorder core can spawn any argv via [`Recorder::spawn`]. This profile
/// is only for [`Recorder::start`], which starts bash in the configured
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
}

/// Bytes inserted into the presentation stream without touching the PTY.
///
/// Presentation output is visible in the final cast, but it is marked
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
        }
    }

    #[must_use]
    pub fn tint_demo() -> Self {
        Self {
            setup_commands: vec!["cd \"$HOME\"".into()],
            prompt: r"\[\e[31m\]t\[\e[33m\]i\[\e[32m\]n\[\e[36m\]t\[\e[0m\] $ ".into(),
            clear_on_start: true,
        }
    }
}

impl Default for ShellProfile {
    fn default() -> Self {
        Self::simple()
    }
}

#[derive(Debug, Clone)]
pub struct RecorderConfig {
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
    /// Bash startup profile for cold `docker run` recordings.
    pub shell: ShellProfile,
    /// Stubbed terminal color query responses.
    pub stubs: StubColors,
    /// Wall-clock guard against hung child processes.
    pub max_runtime: Duration,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 30,
            image: "tint-recorder:demo".into(),
            container: std::env::var("TINT_RECORDER_CONTAINER")
                .ok()
                .filter(|value| !value.is_empty()),
            warm_command: vec!["tint-recorder-shell".into()],
            shell: ShellProfile::tint_demo(),
            stubs: StubColors::default(),
            max_runtime: Duration::from_mins(4),
        }
    }
}

pub struct Recorder {
    cfg: RecorderConfig,
    pty: PtyMaster,
    drainer: Drainer,
    /// Hold the cold `docker run` rcfile alive — `Drop` unlinks it.
    _rcfile: Option<NamedTempFile>,
    recording: RecordingBuilder,
    started_at: Instant,
}

impl Recorder {
    /// Spawn an arbitrary interactive process under a PTY.
    ///
    /// This is the reusable-library entry point: callers own the child process,
    /// environment, shell profile, and any domain-specific setup. The recorder
    /// owns PTY IO, terminal geometry, OSC stubbing, and deterministic cast
    /// timestamps.
    ///
    /// # Errors
    /// Empty argv, or `forkpty` / `execvp(argv[0])` failure.
    pub fn spawn(cfg: RecorderConfig, argv: &[&str]) -> anyhow::Result<Self> {
        let pty = spawn_pty(argv, cfg.cols, cfg.rows).context("spawn process")?;
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
    pub fn start(cfg: RecorderConfig) -> anyhow::Result<Self> {
        if cfg.container.is_some() && cfg.warm_command.is_empty() {
            anyhow::bail!("warm recorder requires at least one warm_command arg");
        }
        let rcfile = if cfg.container.is_some() {
            None
        } else {
            Some(build_rcfile(&cfg.shell).context("write rcfile")?)
        };
        let lines_env = format!("LINES={}", cfg.rows);
        let cols_env = format!("COLUMNS={}", cfg.cols);
        let home_env;
        let mount;
        let argv: Vec<String> = if let Some(container) = &cfg.container {
            let seq = CONTAINER_HOME_SEQ.fetch_add(1, Ordering::Relaxed);
            let home = format!(
                "/home/demo/.tint-recorder-home-{}-{seq}",
                std::process::id(),
            );
            home_env = format!("HOME={home}");
            let mut argv = vec![
                "docker".into(),
                "exec".into(),
                "-i".into(),
                "-t".into(),
                "-e".into(),
                lines_env.clone(),
                "-e".into(),
                cols_env.clone(),
                "-e".into(),
                home_env,
            ];
            argv.push(container.clone());
            argv.extend(cfg.warm_command.iter().cloned());
            argv
        } else {
            let Some(rcfile) = &rcfile else {
                anyhow::bail!("cold recorder missing rcfile");
            };
            mount = format!("{}:/tmp/recorderrc:ro", rcfile.path().display());
            let mut argv = vec![
                "docker".into(),
                "run".into(),
                "--rm".into(),
                "-i".into(),
                "-t".into(),
                "-e".into(),
                lines_env,
                "-e".into(),
                cols_env,
            ];
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
        };
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let pty = spawn_pty(&argv_refs, cfg.cols, cfg.rows).context("spawn docker run")?;
        Ok(Self::from_pty(cfg, pty, rcfile))
    }

    fn from_pty(cfg: RecorderConfig, pty: PtyMaster, rcfile: Option<NamedTempFile>) -> Self {
        let drainer = Drainer::start(pty.fd(), cfg.stubs);
        Self {
            cfg,
            pty,
            drainer,
            _rcfile: rcfile,
            recording: RecordingBuilder::new(),
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
    /// As [`Recorder::dwell`].
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
    /// As [`Recorder::dwell`].
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
    /// As [`Recorder::dwell`].
    pub fn keys(&mut self, key: Key, dwell: Duration, repeat: usize) -> anyhow::Result<()> {
        for _ in 0..repeat {
            self.key(key, dwell)?;
        }
        Ok(())
    }

    /// Send repeated raw key bytes as one live burst while advancing cast time
    /// as if each key happened at `dwell` cadence.
    ///
    /// This is the virtual-time form of [`Recorder::keys`]: useful when the
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
            .record_step(input, captured, DwellMs::from_duration(total_dwell))?;
        Ok(())
    }

    /// Add presentation output directly to the cast without touching the PTY.
    ///
    /// This is the low-level presentation-time escape hatch. The output is
    /// visible in the cast and explicitly marked as presentation output in
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
        self.recording.record_presentation_output(
            output.into().into_bytes(),
            DwellMs::from_duration(dwell),
        )?;
        Ok(())
    }

    /// Advance cast presentation time without waiting on the child process.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`.
    pub fn advance_virtual_time(&mut self, dwell: Duration) -> anyhow::Result<()> {
        self.push_presentation_output(Vec::new(), dwell)
    }

    /// Type text in the cast without sending it to the shell.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`.
    pub fn type_presentation_text(&mut self, text: &str, per_char: Duration) -> anyhow::Result<()> {
        self.check_runtime()?;
        for ch in text.chars() {
            let mut buf = [0_u8; 4];
            self.recording.record_presentation_output(
                ch.encode_utf8(&mut buf).as_bytes().to_vec(),
                DwellMs::from_duration(per_char),
            )?;
        }
        Ok(())
    }

    /// Type a UTF-8 string, one codepoint at a time, dwelling `per_char`
    /// after each byte sequence.
    ///
    /// # Errors
    /// As [`Recorder::dwell`].
    pub fn type_text(&mut self, text: &str, per_char: Duration) -> anyhow::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        self.check_runtime()?;
        let pending = self.drainer.consume();
        if !pending.is_empty() {
            self.recording
                .record_output(pending, DwellMs::from_duration(Duration::ZERO))?;
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
                DwellMs::from_duration(Duration::ZERO),
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
                    DwellMs::from_duration(per_char),
                )?;
                is_first_char = false;
            } else {
                self.recording
                    .record_output(bytes.to_vec(), DwellMs::from_duration(per_char))?;
            }
        }

        let echo_end = echo_start + expected.len();
        if echo_end < captured.len() {
            self.recording.record_output(
                captured[echo_end..].to_vec(),
                DwellMs::from_duration(Duration::ZERO),
            )?;
        }
        Ok(())
    }

    /// Send raw bytes (escape sequences, control codes) with the given dwell.
    ///
    /// # Errors
    /// As [`Recorder::dwell`].
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
    /// Cast time is **not** advanced by waiting; bytes that arrive
    /// during the wait are folded into the next `dwell`/`key` event.
    /// Combine with a small explicit `dwell` for cast-side visible
    /// time:
    ///
    /// ```ignore
    /// let alt_in = r.arm_watch(b"\x1b[?1049h");
    /// r.type_text("tint", TYPE_COMMAND)?;
    /// r.key(Key::Enter, ms(0))?;
    /// alt_in.wait(PICKER_STARTUP_TIMEOUT).expect("picker startup");
    /// r.dwell(PICKER_STARTUP_VISIBLE, ms(0))?;   // small cast-time buffer
    /// ```
    ///
    /// When `TINT_RECORDER_PROFILE=1` is set in the environment,
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

    /// Capture bytes after a real-time settle without creating a playback
    /// event. The settle is capture latency only.
    ///
    /// # Errors
    /// Recording exceeded `max_runtime`.
    pub fn capture_after(&mut self, settle: Duration) -> anyhow::Result<Vec<u8>> {
        self.check_runtime()?;
        std::thread::sleep(settle);
        Ok(self.drainer.consume())
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
            .record_step(bytes.to_vec(), captured, DwellMs::from_duration(dwell))?;
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
        self.recording
            .record_step(bytes.to_vec(), captured, DwellMs::from_duration(dwell))?;
        Ok(elapsed)
    }

    fn check_runtime(&self) -> anyhow::Result<()> {
        if self.started_at.elapsed() > self.cfg.max_runtime {
            anyhow::bail!(
                "recording exceeded max_runtime={}ms (child hung or scene too long?)",
                self.cfg.max_runtime.as_millis(),
            );
        }
        Ok(())
    }

    /// Terminate the child and stop the drainer. The recorder is consumed —
    /// call `write_cast` afterwards via `into_cast`.
    ///
    /// # Errors
    /// SIGKILL or waitpid failed (other than `ESRCH`, which is treated
    /// as already-exited).
    pub fn stop(self) -> anyhow::Result<Cast> {
        let cast = self
            .recording
            .finish_synthetic(self.cfg.cols, self.cfg.rows)?
            .into_cast();
        self.pty.terminate_child()?;
        // Drop fields in order: drainer joins, _rcfile unlinks.
        Ok(cast)
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
        .prefix("tint-recorder-rc-")
        .suffix(".rc")
        .tempfile()?;
    for line in &profile.setup_commands {
        writeln!(f, "{line}")?;
    }
    writeln!(f, "PS1={}", shell_single_quote(&profile.prompt))?;
    if profile.clear_on_start {
        writeln!(f, "printf '\\033[H\\033[2J\\033[3J'")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let cfg = RecorderConfig::default();
        assert_eq!(cfg.cols, 80);
        assert_eq!(cfg.rows, 30);
        assert_eq!(cfg.image, "tint-recorder:demo");
        assert_eq!(cfg.warm_command, ["tint-recorder-shell"]);
        assert_eq!(cfg.shell, ShellProfile::tint_demo());
    }

    #[test]
    fn rcfile_contains_cd_and_ps1_and_clear() {
        let f = build_rcfile(&ShellProfile::tint_demo()).unwrap();
        let s = std::fs::read_to_string(f.path()).unwrap();
        assert!(s.contains("cd \"$HOME\""));
        assert!(s.contains("PS1="));
        assert!(s.contains("printf"));
    }

    #[test]
    fn rcfile_uses_generic_shell_profile() {
        let f = build_rcfile(&ShellProfile {
            setup_commands: vec!["cd /work".into(), "export DEMO=1".into()],
            prompt: "demo's $ ".into(),
            clear_on_start: false,
        })
        .unwrap();
        let s = std::fs::read_to_string(f.path()).unwrap();
        assert!(s.contains("cd /work\n"));
        assert!(s.contains("export DEMO=1\n"));
        assert!(s.contains("PS1='demo'\\''s $ '\n"));
        assert!(!s.contains("\\033[H"));
        assert!(!s.contains("tint"));
    }

    #[test]
    fn shell_quote_handles_empty_and_plain_values() {
        assert_eq!(shell_single_quote(""), "''");
        assert_eq!(shell_single_quote("$ "), "'$ '");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }
}
