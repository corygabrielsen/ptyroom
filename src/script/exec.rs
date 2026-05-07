//! Execute a parsed [`Script`] against the recorder library.
//!
//! Maps each AST [`Action`] to one or more recorder calls, then
//! returns the assembled [`Trace`]. Errors carry the source line number
//! from the AST for diagnostic clarity.

use std::time::Duration;

use anyhow::{Context, anyhow};

use crate::pty::{PtyTracer, PtyTracerConfig, ShellProfile};
use crate::trace::Trace;

use super::ast::{Action, Config, Located, Script, SpawnTarget};

const DEFAULT_WAITFOR_TIMEOUT: Duration = Duration::from_secs(2);

impl Script {
    /// Run the script to completion, returning the produced [`Trace`].
    ///
    /// # Errors
    /// PTY spawn / docker invocation failure, `WaitFor` timeout (with
    /// script line number in the message), or any underlying recorder
    /// error.
    pub fn run(self) -> anyhow::Result<Trace> {
        let cfg = build_recorder_config(&self.config);
        let mut rec = match self.config.spawn.clone() {
            SpawnTarget::Spawn(argv) => {
                let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                PtyTracer::spawn(cfg, &argv_refs).context("script: PtyTracer::spawn failed")?
            }
            SpawnTarget::Warm(_) | SpawnTarget::Cold(_) => {
                PtyTracer::start(cfg).context("script: PtyTracer::start failed")?
            }
        };

        for located in &self.body {
            execute_action(&mut rec, located, &self.config)
                .with_context(|| format!("script:{}", located.line))?;
        }

        rec.stop()
    }
}

fn build_recorder_config(script_config: &Config) -> PtyTracerConfig {
    let mut cfg = PtyTracerConfig {
        cols: script_config.cols,
        rows: script_config.rows,
        max_runtime: script_config.max_runtime,
        ..PtyTracerConfig::default()
    };

    match &script_config.spawn {
        SpawnTarget::Spawn(_) => {
            // Local spawn — PtyTracer::spawn handles argv directly; we
            // don't pass anything here. Container/image fields ignored.
        }
        SpawnTarget::Warm(name) => {
            cfg.container = Some(name.clone());
            if let Some(cmd) = &script_config.warm_command {
                cfg.warm_command.clone_from(cmd);
            }
        }
        SpawnTarget::Cold(image) => {
            cfg.image.clone_from(image);
            cfg.container = None;
            if let Some(rcfile_bytes) = &script_config.shell_rcfile {
                cfg.shell = ShellProfile {
                    raw_rcfile: Some(rcfile_bytes.clone()),
                    ..ShellProfile::simple()
                };
            }
        }
    }

    // SetEnv currently passes through only for Warm and Cold (which
    // forward via docker -e). Local Spawn ignores it for now.
    // TODO: wire script env into PtyTracer::spawn argv prefix.
    cfg
}

fn execute_action(
    rec: &mut PtyTracer,
    located: &Located<Action>,
    script_config: &Config,
) -> anyhow::Result<()> {
    match &located.value {
        Action::Send(bytes) => {
            rec.write_bytes(bytes)?;
        }
        Action::Press {
            key,
            repeat,
            dwell,
            settle,
        } => {
            let dwell = dwell.unwrap_or(script_config.per_key_dwell);
            for _ in 0..*repeat {
                if let Some(s) = settle {
                    rec.key_settle(*key, dwell, *s)?;
                } else {
                    rec.key(*key, dwell)?;
                }
            }
        }
        Action::Type { text, per_char } => {
            let per_char = per_char.unwrap_or(script_config.per_char_dwell);
            // Script `Type` uses recorder.type_text, which expects str.
            let s = std::str::from_utf8(text)
                .map_err(|_| anyhow!("Type with non-UTF-8 bytes is not supported yet"))?;
            rec.type_text(s, per_char)?;
        }
        Action::WaitFor {
            pattern,
            timeout,
            label,
            dwell,
        } => {
            let timeout = timeout.unwrap_or(DEFAULT_WAITFOR_TIMEOUT);
            let label_ref = label.as_deref().unwrap_or("WaitFor");
            let dwell = dwell.unwrap_or(Duration::ZERO);
            rec.wait_for_regex(pattern, dwell, timeout, label_ref)?;
        }
        Action::Sleep { dwell, settle } => {
            // Sleep advances playback by `dwell`. Optional `settle` is
            // wall-clock time to capture incoming PTY bytes (TUI scripts
            // need this so picker/menu draws land in the trace).
            rec.dwell(*dwell, *settle)?;
        }
        Action::Mark(label) => {
            // Markers are diagnostic-only; record at the current
            // elapsed time. PtyTracer doesn't expose an elapsed-since-
            // start helper directly; we use the recording's marker
            // mechanism via push_marker when one becomes available.
            // For v1, log the marker to stderr if PROFILE is set.
            if std::env::var_os("PTYTRACE_PROFILE").is_some() {
                eprintln!("[script] mark {label}");
            }
        }
        Action::Present(bytes) => {
            // Present synthetic output via the recorder's
            // push_presentation_output helper.
            // Default dwell ZERO; subsequent Sleep extends it.
            rec.push_presentation_output(bytes.clone(), Duration::ZERO)?;
        }
        Action::PresentTyped { text, per_char } => {
            let per_char = per_char.unwrap_or(script_config.per_char_dwell);
            let s = std::str::from_utf8(text)
                .map_err(|_| anyhow!("PresentTyped with non-UTF-8 bytes is not supported yet"))?;
            rec.type_presentation_text(s, per_char)?;
        }
    }
    Ok(())
}
