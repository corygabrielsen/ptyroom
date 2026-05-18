//! Geometry-change reporting from join client to host.
//!
//! [`send_resize_if_changed`] is the de-duping shim that turns a
//! per-tick "current viewport size" sample into at most one resize
//! frame per change. The relay coordinator calls it on every loop turn
//! so resize bursts collapse into a single upstream frame.

use std::os::fd::RawFd;

use super::super::room_protocol::{self, TerminalSize};
use super::super::terminal_io::write_all;

pub(super) fn send_resize_if_changed(
    stream_fd: RawFd,
    size: Option<TerminalSize>,
    last_size: &mut Option<TerminalSize>,
) -> anyhow::Result<()> {
    let Some(size) = size else {
        return Ok(());
    };
    if Some(size) == *last_size {
        return Ok(());
    }
    let frame = room_protocol::encode_resize_control(size);
    write_all(stream_fd, &frame)?;
    *last_size = Some(size);
    Ok(())
}
