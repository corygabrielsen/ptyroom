//! Raw-mode guard for `ptyroom` client stdin.

use std::os::fd::{BorrowedFd, RawFd};

use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};

pub(super) use super::super::terminal_io::{terminal_size, write_all};

pub(super) struct RawModeGuard {
    fd: RawFd,
    original: Termios,
}

impl RawModeGuard {
    pub(super) fn enter(fd: RawFd) -> anyhow::Result<Self> {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let original = tcgetattr(borrowed)?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        tcsetattr(borrowed, SetArg::TCSAFLUSH, &raw)?;
        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = tcsetattr(borrowed, SetArg::TCSAFLUSH, &self.original);
    }
}
