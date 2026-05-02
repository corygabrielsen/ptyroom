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
            max_runtime: Duration::from_secs(240),
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

    pub fn event_count(&self) -> usize { self.events.len() }
    pub fn cols(&self) -> u16 { self.cfg.cols }
    pub fn rows(&self) -> u16 { self.cfg.rows }

    /// Dwell at the current state for `dwell` of playback time, with `settle`
    /// of real wall-clock time to let the container produce output.
    pub fn dwell(&mut self, dwell: Duration, settle: Duration) -> anyhow::Result<()> {
        self.send_and_capture(&[], dwell, settle)
    }

    /// Send a single key, dwell `dwell` after.
    pub fn key(&mut self, key: Key, dwell: Duration) -> anyhow::Result<()> {
        self.send_and_capture(key.bytes(), dwell, Duration::from_millis(100))
    }

    /// Send a key `repeat` times, with `dwell` between each.
    pub fn keys(&mut self, key: Key, dwell: Duration, repeat: usize) -> anyhow::Result<()> {
        for _ in 0..repeat { self.key(key, dwell)?; }
        Ok(())
    }

    /// Type a UTF-8 string, one codepoint at a time, dwelling `per_char`
    /// after each byte sequence.
    pub fn type_text(&mut self, text: &str, per_char: Duration) -> anyhow::Result<()> {
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            let bytes = ch.encode_utf8(&mut buf).as_bytes();
            self.send_and_capture(bytes, per_char, Duration::from_millis(100))?;
        }
        Ok(())
    }

    /// Send raw bytes (escape sequences, control codes) with the given dwell.
    pub fn send_raw(&mut self, bytes: &[u8], dwell: Duration) -> anyhow::Result<()> {
        self.send_and_capture(bytes, dwell, Duration::from_millis(100))
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
        for ev in &self.events {
            if !ev.output.is_empty() {
                let data = String::from_utf8_lossy(&ev.output).into_owned();
                events.push(CastEvent {
                    time_s: t_ms as f64 / 1000.0,
                    kind: EventKind::Output,
                    data,
                });
            }
            t_ms = t_ms.saturating_add(ev.dwell.as_millis() as u64);
        }
        Cast { header, events }
    }
}

fn write_all(fd: i32, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("pty write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Write the rcfile bash will source at startup. Two responsibilities:
/// pin PWD to `$HOME` so the cd hook can't escape the demo container, and
/// install a colored PS1 that visibly reflects ANSI palette changes.
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
