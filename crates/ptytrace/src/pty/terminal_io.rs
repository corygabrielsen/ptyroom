//! Raw-fd terminal helpers shared by `ptyroom` host and client viewports.

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

pub(crate) fn write_all(fd: RawFd, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("ptyroom viewport write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("ptyroom viewport write failed: {err}")),
        }
    }
    Ok(())
}
