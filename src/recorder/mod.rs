//! Recorder library.
//!
//! Spawns bash inside a Docker container via PTY, drives the demo by
//! sending scripted keystrokes/dwells, captures every byte written, and
//! emits an asciinema v2 cast with deterministic timestamps.
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
use std::time::{Duration, Instant};

use anyhow::Context;
use nix::unistd::write;
use tempfile::NamedTempFile;

use crate::cast::{Cast, CastEvent, CastHeader, EventKind};
use drainer::Drainer;
use pty::{PtyMaster, spawn};

/// Wall-clock window we wait after each input write to let the container
/// produce output before we drain. Long enough for the picker's render
/// pipeline; short enough that scenes don't run for ages on the host.
const DEFAULT_SETTLE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub cols: u16,
    pub rows: u16,
    pub image: String,
    pub stubs: StubColors,
    pub max_runtime: Duration,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            cols: 80, rows: 30,
            image: "tint-recorder:demo".into(),
            stubs: StubColors::default(),
            max_runtime: Duration::from_mins(4),
        }
    }
}

/// One captured event: the bytes the container wrote in response to our
/// preceding action, plus the playback dwell that follows.
#[derive(Debug, Clone)]
struct RecordedEvent {
    output: Vec<u8>,
    dwell: Duration,
}

pub struct Recorder {
    cfg: RecorderConfig,
    pty: PtyMaster,
    drainer: Drainer,
    /// Hold the rcfile alive — `Drop` unlinks the host tempfile.
    _rcfile: NamedTempFile,
    events: Vec<RecordedEvent>,
    started_at: Instant,
}

impl Recorder {
    /// Spawn `docker run` whose container runs interactive bash.
    /// The docker-side TTY size is set via `-e LINES/COLUMNS` and inherited
    /// from the host PTY's winsize (TIOCSWINSZ via `forkpty`).
    ///
    /// # Errors
    /// rcfile write fails, or `forkpty` / `execvp("docker")` fails.
    pub fn start(cfg: RecorderConfig) -> anyhow::Result<Self> {
        let rcfile = build_rcfile().context("write rcfile")?;
        let lines_env = format!("LINES={}", cfg.rows);
        let cols_env  = format!("COLUMNS={}", cfg.cols);
        let mount     = format!("{}:/tmp/recorderrc:ro", rcfile.path().display());
        let argv: [&str; 16] = [
            "docker", "run", "--rm", "-i", "-t",
            "-e", &lines_env,
            "-e", &cols_env,
            "-v", &mount,
            &cfg.image,
            "bash", "--rcfile", "/tmp/recorderrc", "-i",
        ];
        let pty = spawn(&argv, cfg.cols, cfg.rows).context("spawn docker run")?;
        let drainer = Drainer::start(pty.fd(), cfg.stubs);
        Ok(Self {
            cfg, pty, drainer, _rcfile: rcfile,
            events: Vec::new(),
            started_at: Instant::now(),
        })
    }

    #[must_use] 
    pub fn event_count(&self) -> usize { self.events.len() }
    #[must_use] 
    pub fn cols(&self) -> u16 { self.cfg.cols }
    #[must_use] 
    pub fn rows(&self) -> u16 { self.cfg.rows }

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

    /// Send a key `repeat` times, with `dwell` between each.
    ///
    /// # Errors
    /// As [`Recorder::dwell`].
    pub fn keys(&mut self, key: Key, dwell: Duration, repeat: usize) -> anyhow::Result<()> {
        for _ in 0..repeat { self.key(key, dwell)?; }
        Ok(())
    }

    /// Type a UTF-8 string, one codepoint at a time, dwelling `per_char`
    /// after each byte sequence.
    ///
    /// # Errors
    /// As [`Recorder::dwell`].
    pub fn type_text(&mut self, text: &str, per_char: Duration) -> anyhow::Result<()> {
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            let bytes = ch.encode_utf8(&mut buf).as_bytes();
            self.send_and_capture(bytes, per_char, DEFAULT_SETTLE)?;
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
    /// r.type_text("tint", TYPE_NORMAL)?;
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

    fn send_and_capture(
        &mut self, bytes: &[u8], dwell: Duration, settle: Duration,
    ) -> anyhow::Result<()> {
        if self.started_at.elapsed() > self.cfg.max_runtime {
            anyhow::bail!(
                "recording exceeded max_runtime={}ms (child hung or scene too long?)",
                self.cfg.max_runtime.as_millis(),
            );
        }
        if !bytes.is_empty() {
            write_all(self.pty.fd(), bytes)?;
        }
        std::thread::sleep(settle);
        let captured = self.drainer.consume();
        self.events.push(RecordedEvent { output: captured, dwell });
        Ok(())
    }

    /// Terminate the child and stop the drainer. The recorder is consumed —
    /// call `write_cast` afterwards via `into_cast`.
    ///
    /// # Errors
    /// SIGKILL or waitpid failed (other than `ESRCH`, which is treated
    /// as already-exited).
    pub fn stop(self) -> anyhow::Result<Cast> {
        let cast = self.build_cast();
        self.pty.terminate_child()?;
        // Drop fields in order: drainer joins, _rcfile unlinks.
        Ok(cast)
    }

    fn build_cast(&self) -> Cast {
        let header = CastHeader {
            version: 2,
            width:  u32::from(self.cfg.cols),
            height: u32::from(self.cfg.rows),
            env: [("TERM".into(), "xterm-256color".into()),
                  ("SHELL".into(), "/bin/bash".into())].into_iter().collect(),
        };
        let mut events = Vec::new();
        let mut t_ms: u64 = 0;
        let mut last_output_t_ms: u64 = 0;
        for ev in &self.events {
            if !ev.output.is_empty() {
                let data = String::from_utf8_lossy(&ev.output).into_owned();
                events.push(CastEvent {
                    time_s: ms_to_seconds(t_ms),
                    kind: EventKind::Output,
                    data,
                });
                last_output_t_ms = t_ms;
            }
            t_ms = t_ms.saturating_add(duration_to_ms(ev.dwell));
        }
        // Trailing-dwell preservation. Empty-output events are dropped from
        // the cast, so any dwell after the final real event was previously
        // lost. The downstream xterm.js replayer (snapshot.ts) gives the
        // last cast event a hardcoded 1s dwell — which silently capped the
        // recorder's outro dwell at 1s no matter what scene authors wrote.
        // Fix: emit a synthetic empty-data terminal event at the final
        // cumulative timestamp. xterm.js's term.write("") is a no-op
        // visually, but the timestamp difference between the last real
        // event and this synthetic one becomes the last frame's dwell.
        if t_ms > last_output_t_ms && !events.is_empty() {
            events.push(CastEvent {
                time_s: ms_to_seconds(t_ms),
                kind: EventKind::Output,
                data: String::new(),
            });
        }
        Cast { header, events }
    }
}

/// `Duration::as_millis` returns `u128`; we saturate to `u64` for cast
/// timestamps which are bounded well below `u64::MAX` ms (~584 million years).
fn duration_to_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Convert a `u64` ms count to a `f64` seconds value. Lossy above 2^53 ms,
/// which is ~285 thousand years — not a concern for any cast we record.
#[allow(clippy::cast_precision_loss)]
fn ms_to_seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
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

/// Write the rcfile bash will source at startup. Two responsibilities:
/// pin PWD to `$HOME` so the cd hook can't escape the demo container, and
/// install a colored PS1 that visibly reflects ANSI palette changes.
///
/// # Errors
/// IO error creating or writing the temp file.
fn build_rcfile() -> std::io::Result<NamedTempFile> {
    use std::io::Write;
    // PS1 spells "tint" in red/yellow/green/cyan (ANSI 1/3/2/6) followed by
    // "$ ". Themes set ANSI 0-15 via OSC 4, so the prompt's letter colors
    // visibly change across themes.
    let ps1 = r"\[\e[31m\]t\[\e[33m\]i\[\e[32m\]n\[\e[36m\]t\[\e[0m\] $ ";
    let mut f = tempfile::Builder::new()
        .prefix("tint-recorder-rc-").suffix(".rc")
        .tempfile()?;
    writeln!(f, "cd \"$HOME\"")?;
    writeln!(f, "PS1='{ps1}'")?;
    writeln!(f, "clear")?;
    f.flush()?;
    Ok(f)
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
    }

    #[test]
    fn rcfile_contains_cd_and_ps1_and_clear() {
        let f = build_rcfile().unwrap();
        let s = std::fs::read_to_string(f.path()).unwrap();
        assert!(s.contains("cd \"$HOME\""));
        assert!(s.contains("PS1="));
        assert!(s.contains("clear"));
    }
}
