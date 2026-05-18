//! Stdin → server-socket data path for the join/watch client loop.
//!
//! [`drain_join_stdin`] is the per-tick stdin handler called by the
//! relay coordinator. It reads pending stdin bytes and either:
//!
//!   * routes them through the local-control parser
//!     ([`handle_local_input`]) when interactive escapes are armed, or
//!   * forwards them straight to the server socket when input is
//!     simple passthrough.
//!
//! [`maybe_flush_remote_input`] and [`flush_remote_input`] gate the
//! actual write to the server socket on `forward_input`, so the watch
//! mode never sends bytes upstream even when the local-control parser
//! produces remote-bound output.

use std::net::{Shutdown, TcpStream};
use std::os::fd::{BorrowedFd, RawFd};

use anyhow::anyhow;
use nix::errno::Errno;
use nix::poll::PollFlags;
use nix::unistd::read;

use super::super::input_router::{LocalInputAction, LocalInputRouter, LocalStatus};
use super::super::terminal_io::write_all;
use super::super::terminal_state::termination_requested;
use super::RelayOpts;
use super::output::OutputSink;

pub(super) struct JoinStdin<'a> {
    pub(super) stream: &'a TcpStream,
    pub(super) stdin_fd: RawFd,
    pub(super) stream_fd: RawFd,
    pub(super) stdout_fd: RawFd,
    pub(super) output: &'a mut OutputSink,
    pub(super) input_router: &'a mut LocalInputRouter,
    pub(super) opts: RelayOpts,
    pub(super) reports_size: bool,
}

pub(super) fn drain_join_stdin(
    revents: PollFlags,
    stdin_open: &mut bool,
    io: JoinStdin<'_>,
    buf: &mut [u8],
) -> anyhow::Result<bool> {
    let JoinStdin {
        stream,
        stdin_fd,
        stream_fd,
        stdout_fd,
        output,
        input_router,
        opts,
        reports_size,
    } = io;
    if !*stdin_open || !revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
        return Ok(true);
    }
    let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
    match read(stdin_borrow, buf) {
        Ok(0) => {
            *stdin_open = false;
            if opts.forward_input && !reports_size {
                let _ = stream.shutdown(Shutdown::Write);
            }
        }
        Ok(n) if opts.local_controls => {
            if !handle_local_input(
                &buf[..n],
                input_router,
                stream_fd,
                stdout_fd,
                output,
                opts.forward_input,
            )? {
                let _ = stream.shutdown(Shutdown::Both);
                return Ok(false);
            }
        }
        Ok(n) if opts.forward_input => write_all(stream_fd, &buf[..n])?,
        Err(Errno::EINTR) if termination_requested() => return Ok(false),
        Ok(_) | Err(Errno::EINTR) => {}
        Err(err) => return Err(anyhow!("read stdin: {err}")),
    }
    Ok(true)
}

fn handle_local_input(
    bytes: &[u8],
    router: &mut LocalInputRouter,
    stream_fd: RawFd,
    stdout_fd: RawFd,
    output: &mut OutputSink,
    forward_input: bool,
) -> anyhow::Result<bool> {
    let mut remote = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        match router.push(byte) {
            LocalInputAction::Remote(byte) => remote.push(byte),
            LocalInputAction::SetStatus(status) => {
                maybe_flush_remote_input(stream_fd, &mut remote, forward_input)?;
                output.set_status(stdout_fd, status)?;
            }
            LocalInputAction::ForceRedraw => {
                maybe_flush_remote_input(stream_fd, &mut remote, forward_input)?;
                output.set_status(stdout_fd, LocalStatus::Connected)?;
                output.force_redraw(stdout_fd)?;
            }
            LocalInputAction::Disconnect => {
                maybe_flush_remote_input(stream_fd, &mut remote, forward_input)?;
                return Ok(false);
            }
            LocalInputAction::UnknownCommand(byte) => {
                output.set_status(stdout_fd, LocalStatus::Connected)?;
                remote.push(byte);
            }
        }
    }
    maybe_flush_remote_input(stream_fd, &mut remote, forward_input)?;
    Ok(true)
}

fn maybe_flush_remote_input(
    stream_fd: RawFd,
    remote: &mut Vec<u8>,
    forward_input: bool,
) -> anyhow::Result<()> {
    if forward_input {
        flush_remote_input(stream_fd, remote)
    } else {
        remote.clear();
        Ok(())
    }
}

fn flush_remote_input(stream_fd: RawFd, remote: &mut Vec<u8>) -> anyhow::Result<()> {
    if remote.is_empty() {
        return Ok(());
    }
    write_all(stream_fd, remote.as_slice())?;
    remote.clear();
    Ok(())
}
