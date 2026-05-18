//! Server-socket → stdout data path for the join/watch client loop.
//!
//! [`drain_join_stream`] is the per-tick socket handler called by the
//! relay coordinator. It reads pending socket bytes, decodes them into
//! [`ServerEvent`]s via the framing parser, and dispatches each event:
//! protocol-hello arms the loop, output bytes go to the output sink,
//! resize events resize the sink.
//!
//! [`ensure_protocol_ready`] is the bouncer — any data event seen
//! before the hello arrives is treated as a protocol violation.

use std::os::fd::{BorrowedFd, RawFd};

use anyhow::anyhow;
use nix::errno::Errno;
use nix::poll::PollFlags;
use nix::unistd::read;

use super::super::room_protocol;
use super::super::terminal_state::termination_requested;
use super::output::OutputSink;
use super::stream::{ServerEvent, ServerStream};

pub(super) fn drain_join_stream(
    revents: PollFlags,
    stream_fd: RawFd,
    stdout_fd: RawFd,
    output: &mut OutputSink,
    protocol_ready: &mut bool,
    server_stream: &mut ServerStream,
    buf: &mut [u8],
) -> anyhow::Result<bool> {
    if revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
        let stream_borrow = unsafe { BorrowedFd::borrow_raw(stream_fd) };
        match read(stream_borrow, buf) {
            Ok(0) | Err(Errno::EIO) => {
                if *protocol_ready {
                    return Ok(false);
                }
                return Err(anyhow!("ptyroom host closed before protocol hello"));
            }
            Ok(n) => handle_server_events(
                server_stream.push(&buf[..n]),
                stdout_fd,
                output,
                protocol_ready,
            )?,
            Err(Errno::EINTR) if termination_requested() => return Ok(false),
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("read ptyroom socket: {err}")),
        }
    }
    Ok(!revents.intersects(PollFlags::POLLERR | PollFlags::POLLNVAL))
}

fn handle_server_events(
    events: Vec<ServerEvent>,
    stdout_fd: RawFd,
    output: &mut OutputSink,
    protocol_ready: &mut bool,
) -> anyhow::Result<()> {
    for event in events {
        match event {
            ServerEvent::Hello(version) => {
                if version != room_protocol::VERSION {
                    return Err(anyhow!(
                        "unsupported ptyroom protocol version {version}; expected {}",
                        room_protocol::VERSION
                    ));
                }
                *protocol_ready = true;
            }
            ServerEvent::Output(bytes) => {
                ensure_protocol_ready(*protocol_ready)?;
                output.write_output(stdout_fd, &bytes)?;
            }
            ServerEvent::Size(size) => {
                ensure_protocol_ready(*protocol_ready)?;
                output.resize(stdout_fd, size)?;
            }
        }
    }
    Ok(())
}

fn ensure_protocol_ready(ready: bool) -> anyhow::Result<()> {
    if ready {
        Ok(())
    } else {
        Err(anyhow!("ptyroom host did not send protocol hello"))
    }
}
