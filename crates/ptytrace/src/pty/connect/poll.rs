//! Per-tick fd polling for the join/watch client loop.
//!
//! Each relay tick polls stdin (when still open) and the server socket
//! in a single syscall. The returned [`JoinPollState`] separates the
//! per-fd revents so `drain_join_stdin` and `drain_join_stream` can
//! react independently without re-deriving slot indices.

use std::os::fd::{BorrowedFd, RawFd};

use anyhow::anyhow;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

use super::RESIZE_CHECK_INTERVAL;

#[derive(Debug)]
pub(super) struct JoinPollState {
    pub(super) stdin_revents: PollFlags,
    pub(super) stream_revents: PollFlags,
}

pub(super) fn poll_join_fds(
    stdin_open: bool,
    stdin_fd: RawFd,
    stream_fd: RawFd,
) -> anyhow::Result<JoinPollState> {
    let stream_borrow = unsafe { BorrowedFd::borrow_raw(stream_fd) };
    let mut fds = Vec::with_capacity(2);
    let stdin_index = stdin_open.then(|| {
        let idx = fds.len();
        let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
        fds.push(PollFd::new(stdin_borrow, PollFlags::POLLIN));
        idx
    });
    let stream_index = fds.len();
    fds.push(PollFd::new(stream_borrow, PollFlags::POLLIN));
    match poll(
        &mut fds,
        PollTimeout::try_from(RESIZE_CHECK_INTERVAL).unwrap_or(PollTimeout::MAX),
    ) {
        Ok(_) => {}
        Err(Errno::EINTR) => {
            return Ok(JoinPollState {
                stdin_revents: PollFlags::empty(),
                stream_revents: PollFlags::empty(),
            });
        }
        Err(err) => return Err(anyhow!("poll ptyroom client: {err}")),
    }

    Ok(JoinPollState {
        stdin_revents: stdin_index
            .and_then(|idx| fds[idx].revents())
            .unwrap_or_else(PollFlags::empty),
        stream_revents: fds[stream_index].revents().unwrap_or_else(PollFlags::empty),
    })
}
