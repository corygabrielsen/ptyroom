//! Fan-out for PTY child output in a share session.
//!
//! When `poll` says the PTY's read side is ready, the share loop
//! pulls bytes and sprays them four ways:
//!
//!   - to the host's stdout (or `HostViewport` if the status bar is
//!     active), so the host can see what the child wrote;
//!   - into the join-replay ring, so late-joining clients see recent
//!     screen state instead of just future bytes;
//!   - across every connected client as a room-protocol `data`
//!     frame, via [`super::client::broadcast`];
//!   - into the recorder's `TraceBuilder` with the wall-clock dwell
//!     since the previous event.
//!
//! [`PtyOutputSinks`] groups the four destinations into one struct
//! so [`handle_pty_revents`] / [`handle_pty_output`] don't take a
//! double-digit argument list. The struct is built fresh per tick
//! from `&mut` fields on `Session`, so it has no state of its own.

use std::os::fd::BorrowedFd;
use std::time::Instant;

use anyhow::anyhow;
use nix::errno::Errno;
use nix::poll::PollFlags;
use nix::unistd::read;

use super::super::room_protocol;
use super::super::terminal_io::write_all;
use super::client::{Client, JoinReplay, ShareStats, broadcast};
use super::host_viewport::HostViewport;
use super::pending::{PendingEvent, PendingState};
use crate::recording::TraceBuilder;

pub(super) struct PtyOutputSinks<'a> {
    pub(super) local_output: bool,
    pub(super) stdout_fd: i32,
    pub(super) host_viewport: Option<&'a mut HostViewport>,
    pub(super) join_replay: &'a mut JoinReplay,
    pub(super) clients: &'a mut Vec<Client>,
    pub(super) stats: &'a mut ShareStats,
}

pub(super) fn handle_pty_revents(
    revents: PollFlags,
    pty_fd: i32,
    buf: &mut [u8],
    sinks: PtyOutputSinks<'_>,
    builder: &mut TraceBuilder,
    pending: &mut PendingState,
) -> anyhow::Result<bool> {
    if revents.intersects(PollFlags::POLLIN) {
        let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        match read(pty_borrow, buf) {
            Ok(0) | Err(Errno::EIO) => return Ok(false),
            Ok(n) => handle_pty_output(&buf[..n], sinks, builder, pending)?,
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("read shared PTY: {err}")),
        }
    }
    Ok(!revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL))
}

fn handle_pty_output(
    bytes: &[u8],
    sinks: PtyOutputSinks<'_>,
    builder: &mut TraceBuilder,
    pending: &mut PendingState,
) -> anyhow::Result<()> {
    let PtyOutputSinks {
        local_output,
        stdout_fd,
        host_viewport,
        join_replay,
        clients,
        stats,
    } = sinks;
    if let Some(viewport) = host_viewport {
        let _ = viewport.process_output(bytes);
    } else if local_output {
        let _ = write_all(stdout_fd, bytes);
    }
    let client_frame = room_protocol::encode_output_frame(bytes);
    join_replay.remember(&client_frame);
    broadcast(clients, &client_frame, stats);
    // Defer the trace record by one event so the dwell attached to
    // this read is `next_arrival - now` — the time it actually stays
    // on screen — rather than `now - last_event`, which would absorb
    // session bootstrap latency into the first event's dwell. See
    // `super::pending` for the contract and `crate::pty::live` (commit
    // `26b840b`) for the live-mode mirror.
    pending.replace(
        PendingEvent::Output(bytes.to_vec()),
        Instant::now(),
        builder,
    )
}
