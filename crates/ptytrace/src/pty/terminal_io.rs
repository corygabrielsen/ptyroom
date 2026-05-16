//! Raw-fd terminal helpers shared across `pty::*`.
//!
//! [`write_all`] is the canonical EINTR-retrying write loop used by
//! every byte-writing surface in `pty/` — share host, live capture,
//! signal-safe restore, recorder driver. Five separate copies of the
//! same loop existed before consolidation; this is the seam.
//! Callers attach call-site context via `.with_context(|| "...")`
//! since the error message here is intentionally generic.

use std::os::fd::{BorrowedFd, RawFd};

use anyhow::anyhow;
use nix::errno::Errno;
use nix::libc;
use nix::unistd::write;

use super::room_protocol::TerminalSize;

pub(crate) fn terminal_size(fd: RawFd) -> Option<TerminalSize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) };
    if rc == 0 && size.ws_col > 0 && size.ws_row > 0 {
        Some(TerminalSize::new(size.ws_col, size.ws_row))
    } else {
        None
    }
}

/// EINTR-retrying write loop over a raw fd. Returns `Ok(())` when
/// every byte has been written; bails on `Ok(0)` (kernel closed
/// the fd mid-write) or any non-EINTR error.
///
/// Generic error messages — callers add their own context with
/// `.with_context(|| "...")` to identify which fd / which surface
/// failed.
///
/// # Errors
/// `write(2)` returned 0 or a non-`EINTR` error.
pub(crate) fn write_all(fd: RawFd, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("write_all: write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("write_all failed: {err}")),
        }
    }
    Ok(())
}
