//! Per-tick fd polling and client servicing for the share host loop.
//!
//! Each Session tick:
//!
//!   1. [`poll_share_fds`] polls listener + PTY + (optional) stdin +
//!      every connected client fd in one syscall, returning a
//!      [`PollState`] of revents per role.
//!   2. [`accept_ready_clients`] drains the listener backlog when its
//!      revents say so, attaching hello + size + replay frames to each
//!      new Client.
//!   3. [`process_client_revents`] walks each existing Client, draining
//!      readable bytes into the PTY and flushing pending bytes out of
//!      the client; clients whose poll surface errored or whose IO
//!      failed are disconnected and counted.
//!
//! These three sit together because they are the share-loop's
//! fd-multiplexing scaffold — separating them from the longer
//! session driver in `mod.rs` makes each step legible in isolation.

use std::net::TcpListener;
use std::os::fd::BorrowedFd;

use anyhow::{Context, anyhow};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

use super::super::room_protocol::{self, TerminalSize};
use super::client::{Client, JoinReplay, ShareStats};

#[derive(Debug)]
pub(super) struct PollState {
    pub(super) listener_readable: bool,
    pub(super) pty_revents: PollFlags,
    pub(super) stdin_revents: PollFlags,
    pub(super) client_revents: Vec<PollFlags>,
}

pub(super) fn poll_share_fds(
    listener_fd: i32,
    pty_fd: i32,
    stdin_fd: Option<i32>,
    clients: &[Client],
) -> anyhow::Result<PollState> {
    let listener_borrow = unsafe { BorrowedFd::borrow_raw(listener_fd) };
    let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
    let mut fds = Vec::with_capacity(2 + usize::from(stdin_fd.is_some()) + clients.len());
    fds.push(PollFd::new(listener_borrow, PollFlags::POLLIN));
    fds.push(PollFd::new(pty_borrow, PollFlags::POLLIN));

    let stdin_index = stdin_fd.map(|fd| {
        let idx = fds.len();
        let stdin_borrow = unsafe { BorrowedFd::borrow_raw(fd) };
        fds.push(PollFd::new(stdin_borrow, PollFlags::POLLIN));
        idx
    });

    let client_start = fds.len();
    for client in clients {
        let client_borrow = unsafe { BorrowedFd::borrow_raw(client.fd()) };
        fds.push(PollFd::new(client_borrow, client.poll_flags()));
    }

    match poll(&mut fds, PollTimeout::from(50_u16)) {
        Ok(_) => {}
        Err(Errno::EINTR) => {
            return Ok(PollState {
                listener_readable: false,
                pty_revents: PollFlags::empty(),
                stdin_revents: PollFlags::empty(),
                client_revents: vec![PollFlags::empty(); clients.len()],
            });
        }
        Err(err) => return Err(anyhow!("poll shared PTY: {err}")),
    }

    let listener_readable = fds[0]
        .revents()
        .is_some_and(|rev| rev.intersects(PollFlags::POLLIN));
    let pty_revents = fds[1].revents().unwrap_or_else(PollFlags::empty);
    let stdin_revents = stdin_index
        .and_then(|idx| fds[idx].revents())
        .unwrap_or_else(PollFlags::empty);
    let client_revents = fds[client_start..]
        .iter()
        .map(|fd| fd.revents().unwrap_or_else(PollFlags::empty))
        .collect();

    Ok(PollState {
        listener_readable,
        pty_revents,
        stdin_revents,
        client_revents,
    })
}

pub(super) fn accept_ready_clients(
    listener: &TcpListener,
    clients: &mut Vec<Client>,
    join_replay: &JoinReplay,
    current_size: TerminalSize,
) -> anyhow::Result<usize> {
    let mut accepted = 0;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let mut client = Client::new(stream)?;
                client.enqueue(&room_protocol::encode_hello_control());
                client.enqueue(&room_protocol::encode_size_control(current_size));
                client.enqueue_replay(join_replay);
                clients.push(client);
                accepted += 1;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(accepted),
            Err(err) => return Err(err).context("accept ptyroom client"),
        }
    }
}

pub(super) fn process_client_revents(
    pty_fd: i32,
    clients: &mut Vec<Client>,
    revents: &[PollFlags],
    stats: &mut ShareStats,
) -> anyhow::Result<()> {
    let mut kept = Vec::with_capacity(clients.len());
    for (idx, mut client) in clients.drain(..).enumerate() {
        let rev = revents.get(idx).copied().unwrap_or_else(PollFlags::empty);
        let mut keep = !rev.intersects(PollFlags::POLLERR | PollFlags::POLLNVAL);
        if keep && client.input_open && rev.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
            client.input_open = client.drain_input_to_pty(pty_fd)?;
        }
        if keep && (rev.intersects(PollFlags::POLLOUT) || client.has_pending()) {
            keep = client.flush_pending();
        }
        if keep {
            kept.push(client);
        } else {
            client.disconnect();
            stats.disconnected += 1;
        }
    }
    *clients = kept;
    Ok(())
}
