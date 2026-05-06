//! Execute a parsed [`Scene`] against the recorder library.
//!
//! Maps each AST [`Action`] to one or more recorder calls, then
//! returns the assembled [`Cast`]. Errors carry the source line number
//! from the AST for diagnostic clarity.

use std::time::Duration;

use anyhow::{Context, anyhow};
use regex::bytes::Regex;

use crate::cast::Cast;
use crate::recorder::{Recorder, RecorderConfig, ShellProfile};

use super::ast::{Action, Config, Located, Scene, SpawnTarget};

const DEFAULT_WAITFOR_TIMEOUT: Duration = Duration::from_secs(2);

impl Scene {
    /// Run the scene to completion, returning the produced [`Cast`].
    ///
    /// # Errors
    /// PTY spawn / docker invocation failure, `WaitFor` timeout (with
    /// scene line number in the message), or any underlying recorder
    /// error.
    pub fn run(self) -> anyhow::Result<Cast> {
        let cfg = build_recorder_config(&self.config);
        let mut rec = match self.config.spawn.clone() {
            SpawnTarget::Spawn(argv) => {
                let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                Recorder::spawn(cfg, &argv_refs).context("scene: Recorder::spawn failed")?
            }
            SpawnTarget::Warm(_) | SpawnTarget::Cold(_) => {
                Recorder::start(cfg).context("scene: Recorder::start failed")?
            }
        };

        for located in &self.body {
            execute_action(&mut rec, located, &self.config)
                .with_context(|| format!("scene:{}", located.line))?;
        }

        rec.stop()
    }
}

fn build_recorder_config(scene_config: &Config) -> RecorderConfig {
    let mut cfg = RecorderConfig {
        cols: scene_config.cols,
        rows: scene_config.rows,
        max_runtime: scene_config.max_runtime,
        ..RecorderConfig::default()
    };

    match &scene_config.spawn {
        SpawnTarget::Spawn(_) => {
            // Local spawn — Recorder::spawn handles argv directly; we
            // don't pass anything here. Container/image fields ignored.
        }
        SpawnTarget::Warm(name) => {
            cfg.container = Some(name.clone());
        }
        SpawnTarget::Cold(image) => {
            cfg.image.clone_from(image);
            cfg.container = None;
            if let Some(rcfile_bytes) = &scene_config.shell_rcfile {
                cfg.shell = ShellProfile {
                    raw_rcfile: Some(rcfile_bytes.clone()),
                    ..ShellProfile::simple()
                };
            }
        }
    }

    // SetEnv currently passes through only for Warm and Cold (which
    // forward via docker -e). Local Spawn ignores it for now.
    // TODO: wire scene env into Recorder::spawn argv prefix.
    cfg
}

fn execute_action(
    rec: &mut Recorder,
    located: &Located<Action>,
    scene_config: &Config,
) -> anyhow::Result<()> {
    match &located.value {
        Action::Send(bytes) => {
            rec.write_bytes(bytes)?;
        }
        Action::Press { key, repeat, dwell } => {
            let dwell = dwell.unwrap_or(scene_config.per_key_dwell);
            for _ in 0..*repeat {
                rec.key(*key, dwell)?;
            }
        }
        Action::Type { text, per_char } => {
            let per_char = per_char.unwrap_or(scene_config.per_char_dwell);
            // Scene `Type` uses recorder.type_text, which expects str.
            let s = std::str::from_utf8(text)
                .map_err(|_| anyhow!("Type with non-UTF-8 bytes is not supported yet"))?;
            rec.type_text(s, per_char)?;
        }
        Action::WaitFor {
            pattern,
            timeout,
            label,
        } => {
            let timeout = timeout.unwrap_or(DEFAULT_WAITFOR_TIMEOUT);
            let label_ref = label.as_deref().unwrap_or("WaitFor");
            wait_for_regex(rec, pattern, timeout, label_ref)?;
        }
        Action::Sleep(dur) => {
            // Sleep extends the most recent event's dwell. The
            // recorder's `dwell` method handles both record-time and
            // beat semantics correctly.
            rec.dwell(*dur, Duration::ZERO)?;
        }
        Action::Mark(label) => {
            // Markers are diagnostic-only; record at the current
            // elapsed time. Recorder doesn't expose an elapsed-since-
            // start helper directly; we use the recording's marker
            // mechanism via push_marker when one becomes available.
            // For v1, log the marker to stderr if PROFILE is set.
            if std::env::var_os("TERM_RECORDER_PROFILE").is_some() {
                eprintln!("[scene] mark {label}");
            }
        }
        Action::Present(bytes) => {
            // Present synthetic output via the recorder's
            // push_presentation_output helper.
            // Default dwell ZERO; subsequent Sleep extends it.
            rec.push_presentation_output(bytes.clone(), Duration::ZERO)?;
        }
    }
    Ok(())
}

/// Block until `pattern` matches in PTY output, then return.
///
/// The recorder's existing `arm_watch` API takes a literal byte
/// pattern. To support regex matching we have to drain bytes,
/// scan with the regex, and either match-and-record or keep waiting.
/// For v1 this is implemented as a polling loop with a small budget.
fn wait_for_regex(
    rec: &mut Recorder,
    pattern: &Regex,
    timeout: Duration,
    label: &str,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let poll_interval = Duration::from_millis(20);
    let mut accumulated: Vec<u8> = Vec::new();
    loop {
        let chunk = rec.capture_after(poll_interval)?;
        accumulated.extend_from_slice(&chunk);
        if pattern.is_match(&accumulated) {
            // Pattern matched. The captured bytes are already recorded
            // by the recorder's capture_after path; we don't need to
            // re-emit. Match found; return success.
            return Ok(());
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "WaitFor /{}/ ({}) timed out after {}ms",
                pattern.as_str(),
                label,
                timeout.as_millis(),
            );
        }
    }
}
