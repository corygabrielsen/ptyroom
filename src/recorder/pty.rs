//! PTY: open a master/slave pair and fork a child whose stdio is the slave.
//!
//! Wraps `nix`'s `forkpty` with typed handles, sets the slave's window size
//! before exec, and exposes the master fd to the parent for IO.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use anyhow::Context;
use nix::libc;
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::{Pid, execvp};

/// Master end of a PTY connected to a child process.
#[derive(Debug)]
pub struct PtyMaster {
    fd: OwnedFd,
    child: Pid,
}

impl PtyMaster {
    #[must_use]
    pub fn fd(&self) -> RawFd { self.fd.as_raw_fd() }

    /// Send SIGKILL to the child and reap it. Idempotent — safe if already dead.
    ///
    /// # Errors
    /// `kill` or `waitpid` returned an error other than `ESRCH` (already gone).
    pub fn terminate_child(&self) -> anyhow::Result<()> {
        match kill(self.child, Signal::SIGKILL) {
            Ok(()) => {}
            Err(nix::errno::Errno::ESRCH) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let _ = waitpid(self.child, Some(WaitPidFlag::empty()));
        Ok(())
    }
}

/// Spawn `argv[0]` with `argv[1..]` as a child whose stdio is a fresh PTY.
/// The slave is sized to `cols × rows` before exec.
///
/// # Errors
/// Empty `argv`, or `forkpty` syscall failure.
pub fn spawn(argv: &[&str], cols: u16, rows: u16) -> anyhow::Result<PtyMaster> {
    if argv.is_empty() { anyhow::bail!("spawn: empty argv"); }
    let winsize = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };

    // SAFETY: forkpty must be called on a single-threaded process or, if not,
    // only async-signal-safe calls are permitted in the child. We restrict
    // the child path to execvp + _exit, both async-signal-safe.
    let res = unsafe { forkpty(Some(&winsize), None) }
        .context("forkpty")?;
    match res {
        ForkptyResult::Parent { master, child } => Ok(PtyMaster { fd: master, child }),
        ForkptyResult::Child => {
            let owned: Vec<std::ffi::CString> = argv.iter()
                .map(|s| std::ffi::CString::new(*s).unwrap_or_else(|_| {
                    // SAFETY: arg with NUL byte — fail loudly via _exit.
                    unsafe { libc::_exit(127) }
                })).collect();
            let arg_refs: Vec<&std::ffi::CStr> = owned.iter().map(std::ffi::CString::as_c_str).collect();
            let _ = execvp(arg_refs[0], &arg_refs);
            // execvp only returns on failure
            unsafe { libc::_exit(127) };
        }
    }
}
