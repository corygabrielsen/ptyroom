//! PTY: open a master/slave pair and spawn a child whose stdio is the slave.
//!
//! Wraps [`portable_pty`] for the platform-correct fork/exec/ctty dance.
//! The master's raw fd is exposed to the existing IO loops (drainer +
//! parent writes) — `portable_pty` owns the master and child handles for
//! their lifetime; the fd stays valid as long as `PtyMaster` is alive.

use std::os::fd::RawFd;

use anyhow::{Context, anyhow};
use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};

pub(super) struct PtyMaster {
    // Field order matters: `child` is killed/reaped via `terminate_child`
    // before drop, then `master` closes the master fd on drop, which sends
    // SIGHUP to any descendants still attached to the slave.
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    fd: RawFd,
}

impl PtyMaster {
    #[must_use]
    pub(super) fn fd(&self) -> RawFd {
        self.fd
    }

    /// Resize the PTY and notify the child side with the platform's
    /// normal terminal-resize semantics.
    ///
    /// # Errors
    /// The platform PTY resize operation failed.
    pub(super) fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()> {
        ensure_nonzero_size(cols, rows)?;
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize PTY")
    }

    /// Send SIGKILL to the child and reap it. Idempotent — safe to call
    /// after the child has already exited.
    pub(super) fn terminate_child(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl std::fmt::Debug for PtyMaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyMaster")
            .field("fd", &self.fd)
            .finish_non_exhaustive()
    }
}

/// Spawn `argv[0]` with `argv[1..]` as a child whose stdio is a fresh
/// PTY sized to `cols × rows`.
///
/// # Errors
/// Empty `argv`, `openpty` failure, child spawn failure, or master fd
/// not exposable as a raw fd.
pub(super) fn spawn(argv: &[&str], cols: u16, rows: u16) -> anyhow::Result<PtyMaster> {
    spawn_with_env(argv, &[], cols, rows)
}

/// Spawn `argv[0]` with additional environment variables.
///
/// Values in `env` override inherited variables with the same name.
///
/// # Errors
/// Empty `argv`, `openpty` failure, child spawn failure, or master fd
/// not exposable as a raw fd.
pub(super) fn spawn_with_env(
    argv: &[&str],
    env: &[(String, String)],
    cols: u16,
    rows: u16,
) -> anyhow::Result<PtyMaster> {
    if argv.is_empty() {
        anyhow::bail!("spawn: empty argv");
    }
    ensure_nonzero_size(cols, rows)?;
    let pty = NativePtySystem::default();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;

    let mut cmd = CommandBuilder::new(argv[0]);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    let child = pair.slave.spawn_command(cmd).context("spawn_command")?;
    drop(pair.slave);

    let fd = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| anyhow!("portable-pty master did not expose a raw fd"))?;

    Ok(PtyMaster {
        child,
        master: pair.master,
        fd,
    })
}

pub(super) fn ensure_nonzero_size(cols: u16, rows: u16) -> anyhow::Result<()> {
    if cols == 0 || rows == 0 {
        anyhow::bail!("PTY size must be nonzero; got {cols}x{rows}");
    }
    Ok(())
}

/// Resolve a PTY child argv: caller-provided argv wins; otherwise fall
/// back to `$SHELL`; final fallback is `bash`. Shared by `pty::live`
/// (capture flow) and `pty::share` (host flow) so both surfaces honor
/// the same SHELL convention.
pub(super) fn resolve_argv(argv: Vec<String>) -> Vec<String> {
    if !argv.is_empty() {
        return argv;
    }
    if let Ok(shell) = std::env::var("SHELL")
        && !shell.is_empty()
    {
        return vec![shell];
    }
    vec!["bash".into()]
}

#[cfg(test)]
mod tests {
    use super::spawn;

    #[test]
    fn spawn_rejects_zero_dimensions() {
        let err = spawn(&["true"], 0, 24).unwrap_err().to_string();

        assert!(err.contains("nonzero"));
    }
}
