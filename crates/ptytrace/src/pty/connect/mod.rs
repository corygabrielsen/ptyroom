//! Client side of a shared PTY session.
//!
//! This is the implementation behind `ptyroom join` and `ptyroom watch`.
//! Interactive stdout renders into an alternate-screen viewport;
//! non-terminal stdout receives decoded PTY output bytes for pipeline use.

use std::io;
use std::io::IsTerminal;
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::time::Duration;

use anyhow::{Context, anyhow};
use nix::errno::Errno;
use nix::poll::PollFlags;
use nix::unistd::read;

use super::input_router::LocalInputRouter;
use super::room_protocol::{self, TerminalSize};
use super::terminal_io::write_all;
use super::terminal_state::{RawModeGuard, termination_requested};

mod drain_stdin;
mod output;
mod poll;
pub mod stream;

use drain_stdin::{JoinStdin, drain_join_stdin};
use output::OutputSink;
use poll::poll_join_fds;
use stream::{ServerEvent, ServerStream};

pub(super) const RESIZE_CHECK_INTERVAL: Duration = Duration::from_millis(250);

/// Connect this process's terminal to a shared PTY server.
///
/// # Errors
/// TCP connection, terminal mode setup, terminal IO, or socket IO failed.
pub fn connect(addr: SocketAddr) -> anyhow::Result<()> {
    connect_with_mode(addr, ClientMode::Join)
}

/// Watch a shared PTY server without sending local input or geometry.
///
/// # Errors
/// TCP connection, terminal mode setup, terminal IO, or socket IO failed.
pub fn watch(addr: SocketAddr) -> anyhow::Result<()> {
    connect_with_mode(addr, ClientMode::Watch)
}

fn connect_with_mode(addr: SocketAddr, mode: ClientMode) -> anyhow::Result<()> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    relay(&stream, mode)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientMode {
    Join,
    Watch,
}

impl ClientMode {
    const fn relay_opts(self, local_controls: bool) -> RelayOpts {
        match self {
            Self::Join => RelayOpts {
                local_controls,
                forward_input: true,
                report_size: true,
            },
            Self::Watch => RelayOpts {
                local_controls,
                forward_input: false,
                report_size: false,
            },
        }
    }

    const fn is_read_only(self) -> bool {
        matches!(self, Self::Watch)
    }
}

fn relay(stream: &TcpStream, mode: ClientMode) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stdin_fd = stdin.as_raw_fd();
    let stdout_fd = stdout.as_raw_fd();
    let stdin_is_terminal = stdin.is_terminal();
    let stdout_is_terminal = stdout.is_terminal();
    let local_controls = local_controls_enabled(stdin_is_terminal, stdout_is_terminal);
    let _raw = if local_controls {
        Some(RawModeGuard::enter(stdin_fd).context("enter raw mode for ptyroom client stdin")?)
    } else {
        None
    };
    let mut output = if stdout_is_terminal {
        OutputSink::viewport(
            stdout_fd,
            addr_label(stream),
            local_controls,
            mode.is_read_only(),
        )?
    } else {
        OutputSink::Raw
    };
    relay_fds_with_output(
        stream,
        stdin_fd,
        stdout_fd,
        &mut output,
        mode.relay_opts(local_controls),
    )
}

const fn local_controls_enabled(stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
    stdin_is_terminal && stdout_is_terminal
}

#[cfg(test)]
fn relay_fds(stream: &TcpStream, stdin_fd: RawFd, stdout_fd: RawFd) -> anyhow::Result<()> {
    let mut output = OutputSink::Raw;
    relay_fds_with_output(
        stream,
        stdin_fd,
        stdout_fd,
        &mut output,
        RelayOpts::join(false),
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RelayOpts {
    pub(super) local_controls: bool,
    pub(super) forward_input: bool,
    pub(super) report_size: bool,
}

impl RelayOpts {
    #[cfg(test)]
    const fn join(local_controls: bool) -> Self {
        Self {
            local_controls,
            forward_input: true,
            report_size: true,
        }
    }

    #[cfg(test)]
    const fn watch(local_controls: bool) -> Self {
        Self {
            local_controls,
            forward_input: false,
            report_size: false,
        }
    }
}

fn relay_fds_with_output(
    stream: &TcpStream,
    stdin_fd: RawFd,
    stdout_fd: RawFd,
    output: &mut OutputSink,
    opts: RelayOpts,
) -> anyhow::Result<()> {
    let stream_fd = stream.as_raw_fd();
    let mut buf = [0_u8; 4096];
    let mut stdin_open = opts.forward_input || opts.local_controls;
    let mut last_size = None;
    let mut protocol_ready = false;
    let mut input_router = LocalInputRouter::default();
    let mut server_stream = ServerStream::default();
    let reports_size = opts.report_size && output.reports_size();
    write_all(stream_fd, &room_protocol::encode_hello_control())?;
    send_resize_if_changed(
        stream_fd,
        reports_size
            .then(|| output.reported_size(stdout_fd))
            .flatten(),
        &mut last_size,
    )?;

    loop {
        if termination_requested() {
            return Ok(());
        }
        send_resize_if_changed(
            stream_fd,
            reports_size
                .then(|| output.reported_size(stdout_fd))
                .flatten(),
            &mut last_size,
        )?;
        let poll_state = poll_join_fds(stdin_open, stdin_fd, stream_fd)?;

        if !drain_join_stdin(
            poll_state.stdin_revents,
            &mut stdin_open,
            JoinStdin {
                stream,
                stdin_fd,
                stream_fd,
                stdout_fd,
                output,
                input_router: &mut input_router,
                opts,
                reports_size,
            },
            &mut buf,
        )? {
            return Ok(());
        }

        if !drain_join_stream(
            poll_state.stream_revents,
            stream_fd,
            stdout_fd,
            output,
            &mut protocol_ready,
            &mut server_stream,
            &mut buf,
        )? {
            return Ok(());
        }
    }
}

fn drain_join_stream(
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

fn send_resize_if_changed(
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

fn addr_label(stream: &TcpStream) -> String {
    stream
        .peer_addr()
        .map_or_else(|_| "room".to_owned(), |addr| addr.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::{AsRawFd, RawFd};
    use std::thread;

    use nix::pty::{Winsize, openpty};
    use nix::unistd::{pipe, read as nix_read, write as nix_write};

    use super::super::input_router::LOCAL_ESCAPE;
    use super::super::room_protocol::{self, TerminalSize};
    use super::output::OutputSink;
    use super::{RelayOpts, local_controls_enabled, relay_fds, relay_fds_with_output};

    #[test]
    fn relay_continues_reading_socket_after_stdin_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut input = Vec::new();
            socket.read_to_end(&mut input).unwrap();
            let mut expected = room_protocol::encode_hello_control();
            expected.extend_from_slice(b"ping");
            assert_eq!(input, expected);
            socket
                .write_all(&room_protocol::encode_hello_control())
                .unwrap();
            socket
                .write_all(&room_protocol::encode_output_frame(b"pong"))
                .unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        let (stdin_r, stdin_w) = pipe().unwrap();
        let (stdout_r, stdout_w) = pipe().unwrap();
        nix_write(&stdin_w, b"ping").unwrap();
        drop(stdin_w);

        relay_fds(&stream, stdin_r.as_raw_fd(), stdout_w.as_raw_fd()).unwrap();
        drop(stdout_w);

        let mut output = Vec::new();
        let mut buf = [0_u8; 16];
        loop {
            match nix_read(&stdout_r, &mut buf).unwrap() {
                0 => break,
                n => output.extend_from_slice(&buf[..n]),
            }
        }
        assert_eq!(output, b"pong");
        server.join().unwrap();
    }

    #[test]
    fn local_escape_dot_disconnects_without_reaching_room() {
        let mut expected = room_protocol::encode_hello_control();
        expected.extend_from_slice(b"abc");

        assert_eq!(collect_join_input_with_mode(b"abc\x1d.def", true), expected);
    }

    #[test]
    fn doubled_local_escape_sends_literal_escape_to_room() {
        let mut expected = room_protocol::encode_hello_control();
        expected.push(LOCAL_ESCAPE);

        assert_eq!(
            collect_join_input_with_mode(&[LOCAL_ESCAPE, LOCAL_ESCAPE], true),
            expected
        );
    }

    #[test]
    fn noninteractive_input_forwards_escape_prefix_literally() {
        let mut expected = room_protocol::encode_hello_control();
        expected.extend_from_slice(&[LOCAL_ESCAPE, b'.', b'\n']);

        assert_eq!(
            collect_join_input_with_mode(&[LOCAL_ESCAPE, b'.', b'\n'], false),
            expected
        );
    }

    #[test]
    fn local_controls_require_interactive_input_and_output() {
        assert!(local_controls_enabled(true, true));
        assert!(!local_controls_enabled(true, false));
        assert!(!local_controls_enabled(false, true));
        assert!(!local_controls_enabled(false, false));
    }

    #[test]
    fn read_only_relay_does_not_forward_input_or_resize() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_hello = room_protocol::encode_hello_control();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_millis(150)))
                .unwrap();
            let mut received = Vec::new();
            let mut buf = [0_u8; 128];
            loop {
                match socket.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => received.extend_from_slice(&buf[..n]),
                    Err(err)
                        if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        break;
                    }
                    Err(err) => panic!("server read failed: {err}"),
                }
            }
            assert_eq!(received, expected_hello);
            socket
                .write_all(&room_protocol::encode_hello_control())
                .unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        let (stdin_r, stdin_w) = pipe().unwrap();
        nix_write(&stdin_w, b"abc\x03\x1b").unwrap();
        drop(stdin_w);
        let winsize = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&winsize), None).unwrap();
        let stdout_fd = pty.slave.as_raw_fd();
        let mut output = OutputSink::viewport(stdout_fd, "test".to_owned(), true, true).unwrap();

        relay_fds_with_output(
            &stream,
            stdin_r.as_raw_fd(),
            stdout_fd,
            &mut output,
            RelayOpts::watch(true),
        )
        .unwrap();

        server.join().unwrap();
    }

    #[test]
    fn local_help_command_is_not_forwarded_and_remote_input_resumes() {
        let mut expected = room_protocol::encode_hello_control();
        expected.extend_from_slice(b"abcd");

        assert_eq!(collect_join_input_with_mode(b"ab\x1d?cd", true), expected);
    }

    #[test]
    fn local_redraw_command_is_not_forwarded_and_remote_input_resumes() {
        let mut expected = room_protocol::encode_hello_control();
        expected.extend_from_slice(b"abcd");

        assert_eq!(collect_join_input_with_mode(b"ab\x1drcd", true), expected);
    }

    #[test]
    fn unknown_local_command_forwards_command_byte_without_prefix() {
        let mut expected = room_protocol::encode_hello_control();
        expected.extend_from_slice(b"axb");

        assert_eq!(collect_join_input_with_mode(b"a\x1dxb", true), expected);
    }

    #[test]
    fn viewport_relay_keeps_write_side_open_after_stdin_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .write_all(&room_protocol::encode_hello_control())
                .unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut input = Vec::new();
            let mut buf = [0_u8; 128];
            while !input.windows(b"ping".len()).any(|window| window == b"ping") {
                let n = socket.read(&mut buf).unwrap();
                assert_ne!(n, 0, "client half-closed before sending piped input");
                input.extend_from_slice(&buf[..n]);
            }

            socket
                .set_read_timeout(Some(std::time::Duration::from_millis(100)))
                .unwrap();
            for _ in 0..3 {
                match socket.read(&mut buf[..1]) {
                    Err(err)
                        if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        return;
                    }
                    Ok(0) => panic!("viewport client half-closed after stdin EOF"),
                    Ok(_) => {}
                    Err(err) => panic!("unexpected socket read error: {err}"),
                }
            }
        });
        let stream = TcpStream::connect(addr).unwrap();
        let (stdin_r, stdin_w) = pipe().unwrap();
        nix_write(&stdin_w, b"ping").unwrap();
        drop(stdin_w);
        let winsize = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&winsize), None).unwrap();
        let stdout_fd = pty.slave.as_raw_fd();
        let mut output = OutputSink::viewport(stdout_fd, "test".to_owned(), true, false).unwrap();

        relay_fds_with_output(
            &stream,
            stdin_r.as_raw_fd(),
            stdout_fd,
            &mut output,
            RelayOpts::join(false),
        )
        .unwrap();

        server.join().unwrap();
    }

    #[test]
    fn viewport_relay_reports_status_reserved_size() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_resize =
            room_protocol::encode_resize_control(TerminalSize { cols: 80, rows: 23 });
        let expected_resize_for_server = expected_resize.clone();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut input = Vec::new();
            let mut buf = [0_u8; 128];
            while !contains_bytes(&input, &expected_resize_for_server) {
                let n = socket.read(&mut buf).unwrap();
                assert_ne!(n, 0, "client closed before reporting viewport size");
                input.extend_from_slice(&buf[..n]);
            }
            assert!(contains_bytes(
                &input,
                &room_protocol::encode_hello_control()
            ));
            socket
                .write_all(&room_protocol::encode_hello_control())
                .unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        let (stdin_r, stdin_w) = pipe().unwrap();
        drop(stdin_w);
        let winsize = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&winsize), None).unwrap();
        let stdout_fd = pty.slave.as_raw_fd();
        let mut output = OutputSink::viewport(stdout_fd, "test".to_owned(), true, false).unwrap();

        relay_fds_with_output(
            &stream,
            stdin_r.as_raw_fd(),
            stdout_fd,
            &mut output,
            RelayOpts::join(false),
        )
        .unwrap();

        server.join().unwrap();
    }

    #[test]
    fn relay_rejects_output_before_protocol_hello() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .write_all(&room_protocol::encode_output_frame(b"pong"))
                .unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        let (stdin_r, stdin_w) = pipe().unwrap();
        let (_stdout_r, stdout_w) = pipe().unwrap();
        drop(stdin_w);

        let err = relay_fds(&stream, stdin_r.as_raw_fd(), stdout_w.as_raw_fd()).unwrap_err();

        assert!(err.to_string().contains("protocol hello"));
        server.join().unwrap();
    }

    #[test]
    fn relay_rejects_unsupported_protocol_version() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.write_all(b"\x1bPptyroom;hello;2\x1b\\").unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        let (stdin_r, stdin_w) = pipe().unwrap();
        let (_stdout_r, stdout_w) = pipe().unwrap();
        drop(stdin_w);

        let err = relay_fds(&stream, stdin_r.as_raw_fd(), stdout_w.as_raw_fd()).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported ptyroom protocol version")
        );
        server.join().unwrap();
    }

    fn collect_join_input_with_mode(input: &[u8], local_controls: bool) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            socket.read_to_end(&mut received).unwrap();
            let _ = socket.write_all(&room_protocol::encode_hello_control());
            received
        });
        let stream = TcpStream::connect(addr).unwrap();
        let (stdin_r, stdin_w) = pipe().unwrap();
        let (_stdout_r, stdout_w) = pipe().unwrap();
        nix_write(&stdin_w, input).unwrap();
        drop(stdin_w);

        if local_controls {
            relay_fds_with_local_controls(&stream, stdin_r.as_raw_fd(), stdout_w.as_raw_fd())
                .unwrap();
        } else {
            relay_fds(&stream, stdin_r.as_raw_fd(), stdout_w.as_raw_fd()).unwrap();
        }

        server.join().unwrap()
    }

    fn relay_fds_with_local_controls(
        stream: &TcpStream,
        stdin_fd: RawFd,
        stdout_fd: RawFd,
    ) -> anyhow::Result<()> {
        let mut output = OutputSink::Raw;
        relay_fds_with_output(
            stream,
            stdin_fd,
            stdout_fd,
            &mut output,
            RelayOpts::join(true),
        )
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
