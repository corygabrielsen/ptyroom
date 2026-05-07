//! PTY: open a master/slave pair and spawn a child whose stdio is the slave.
//!
//! Wraps [`portable_pty`] for the platform-correct fork/exec/ctty dance.
//! The master's raw fd is exposed to the existing IO loops (drainer +
//! parent writes) — `portable_pty` owns the master and child handles for
//! their lifetime; the fd stays valid as long as `PtyMaster` is alive.

use std::os::fd::RawFd;

use anyhow::{Context, anyhow};
use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};

pub struct PtyMaster {
    // Field order matters: `child` is killed/reaped via `terminate_child`
    // before drop, then `master` closes the master fd on drop, which sends
    // SIGHUP to any descendants still attached to the slave.
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    fd: RawFd,
}

impl PtyMaster {
    #[must_use]
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Resize the PTY and notify the child side with the platform's
    /// normal terminal-resize semantics.
    ///
    /// # Errors
    /// The platform PTY resize operation failed.
    pub fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()> {
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
    pub fn terminate_child(&mut self) {
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
pub fn spawn(argv: &[&str], cols: u16, rows: u16) -> anyhow::Result<PtyMaster> {
    if argv.is_empty() {
        anyhow::bail!("spawn: empty argv");
    }
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
