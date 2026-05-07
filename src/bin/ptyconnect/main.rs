//! `ptyconnect` CLI: attach to a `ptyshare` session.

use std::io;
use std::io::IsTerminal;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::time::Duration;

use anyhow::anyhow;
use clap::Parser;
use nix::errno::Errno;
use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{read, write};

const RESIZE_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const MAX_CONTROL_BYTES: usize = 1024;
const MAX_DATA_FRAME_BYTES: usize = 16 * 1024 * 1024;
const CONTROL_PREFIX: &[u8] = b"\x1bPptyshare;";
const CONTROL_SUFFIX: &[u8] = b"\x1b\\";

#[derive(Parser)]
#[command(
    version,
    about = "ptyconnect — connect your terminal to a ptyshare session",
    long_about = "Connect stdin/stdout to a `ptyshare` TCP session. The transport\n\
                  reports local terminal resizes so the shared PTY can use a\n\
                  size every attached client can display. Interactive clients\n\
                  render the shared screen in a local alternate screen. The transport\n\
                  has no built-in auth or encryption; connect through SSH,\n\
                  WireGuard, or another trusted tunnel outside loopback."
)]
struct Args {
    /// ptyshare host:port.
    addr: SocketAddr,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    connect(args.addr)
}

fn connect(addr: SocketAddr) -> anyhow::Result<()> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    relay(&stream)
}

fn relay(stream: &TcpStream) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stdin_fd = stdin.as_raw_fd();
    let stdout_fd = stdout.as_raw_fd();
    let _raw = if stdin.is_terminal() {
        RawModeGuard::enter(stdin_fd).ok()
    } else {
        None
    };
    let mut output = if stdout.is_terminal() {
        OutputSink::Viewport(Box::new(ViewportRenderer::enter(stdout_fd)?))
    } else {
        OutputSink::Raw
    };
    relay_fds_with_output(stream, stdin_fd, stdout_fd, &mut output)
}

#[cfg(test)]
fn relay_fds(stream: &TcpStream, stdin_fd: RawFd, stdout_fd: RawFd) -> anyhow::Result<()> {
    let mut output = OutputSink::Raw;
    relay_fds_with_output(stream, stdin_fd, stdout_fd, &mut output)
}

fn relay_fds_with_output(
    stream: &TcpStream,
    stdin_fd: RawFd,
    stdout_fd: RawFd,
    output: &mut OutputSink,
) -> anyhow::Result<()> {
    let stream_fd = stream.as_raw_fd();
    let mut buf = [0_u8; 4096];
    let mut stdin_open = true;
    let mut last_size = None;
    let mut server_stream = ServerStream::default();
    send_resize_if_changed(stream_fd, stdout_fd, &mut last_size)?;

    loop {
        if stdin_open {
            send_resize_if_changed(stream_fd, stdout_fd, &mut last_size)?;
        }
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
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(anyhow!("poll ptyconnect: {err}")),
        }

        let stdin_revents = stdin_index
            .and_then(|idx| fds[idx].revents())
            .unwrap_or_else(PollFlags::empty);
        if stdin_open && stdin_revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
            let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
            match read(stdin_borrow, &mut buf) {
                Ok(0) => {
                    stdin_open = false;
                    let _ = stream.shutdown(Shutdown::Write);
                }
                Ok(n) => write_all(stream_fd, &buf[..n])?,
                Err(Errno::EINTR) => {}
                Err(err) => return Err(anyhow!("read stdin: {err}")),
            }
        }

        if let Some(rev) = fds[stream_index].revents() {
            if rev.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
                let stream_borrow = unsafe { BorrowedFd::borrow_raw(stream_fd) };
                match read(stream_borrow, &mut buf) {
                    Ok(0) | Err(Errno::EIO) => return Ok(()),
                    Ok(n) => {
                        for event in server_stream.push(&buf[..n]) {
                            match event {
                                ServerEvent::Output(bytes) => {
                                    output.write_output(stdout_fd, &bytes)?;
                                }
                                ServerEvent::Size(size) => {
                                    output.resize(stdout_fd, size)?;
                                }
                            }
                        }
                    }
                    Err(Errno::EINTR) => {}
                    Err(err) => return Err(anyhow!("read ptyshare socket: {err}")),
                }
            }
            if rev.intersects(PollFlags::POLLERR | PollFlags::POLLNVAL) {
                return Ok(());
            }
        }
    }
}

enum OutputSink {
    Raw,
    Viewport(Box<ViewportRenderer>),
}

impl OutputSink {
    fn write_output(&mut self, stdout_fd: RawFd, bytes: &[u8]) -> anyhow::Result<()> {
        match self {
            Self::Raw => write_all(stdout_fd, bytes),
            Self::Viewport(renderer) => renderer.process_output(bytes),
        }
    }

    fn resize(&mut self, stdout_fd: RawFd, size: TerminalSize) -> anyhow::Result<()> {
        match self {
            Self::Raw => Ok(()),
            Self::Viewport(renderer) => renderer.resize(stdout_fd, size),
        }
    }
}

struct ViewportRenderer {
    stdout_fd: RawFd,
    parser: vt100::Parser,
    size: TerminalSize,
}

impl ViewportRenderer {
    fn enter(stdout_fd: RawFd) -> anyhow::Result<Self> {
        let size = terminal_size(stdout_fd).unwrap_or(TerminalSize { cols: 80, rows: 24 });
        write_all(stdout_fd, b"\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J")?;
        Ok(Self {
            stdout_fd,
            parser: vt100::Parser::new(size.rows, size.cols, 0),
            size,
        })
    }

    fn process_output(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.parser.process(bytes);
        self.redraw()
    }

    fn resize(&mut self, stdout_fd: RawFd, size: TerminalSize) -> anyhow::Result<()> {
        if self.size != size {
            self.parser.screen_mut().set_size(size.rows, size.cols);
            self.size = size;
        }
        self.stdout_fd = stdout_fd;
        self.redraw()
    }

    fn redraw(&mut self) -> anyhow::Result<()> {
        let frame = render_viewport(self.parser.screen(), terminal_size(self.stdout_fd));
        write_all(self.stdout_fd, &frame)
    }
}

impl Drop for ViewportRenderer {
    fn drop(&mut self) {
        let _ = write_all(
            self.stdout_fd,
            b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1049l",
        );
    }
}

fn render_viewport(screen: &vt100::Screen, local_size: Option<TerminalSize>) -> Vec<u8> {
    let (rows, cols) = screen.size();
    let local = local_size.unwrap_or(TerminalSize { cols, rows });
    let rows = rows.min(local.rows);
    let cols = cols.min(local.cols);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[H\x1b[2J");
    for (idx, row) in screen.rows_formatted(0, cols).enumerate() {
        let Ok(row_num) = u16::try_from(idx + 1) else {
            break;
        };
        if row_num > rows {
            break;
        }
        out.extend_from_slice(format!("\x1b[{row_num};1H").as_bytes());
        out.extend_from_slice(&row);
    }
    out.extend_from_slice(&screen.input_mode_formatted());
    out.extend_from_slice(&screen.cursor_state_formatted());
    out
}

#[derive(Debug, PartialEq, Eq)]
enum ServerEvent {
    Output(Vec<u8>),
    Size(TerminalSize),
}

#[derive(Debug, Default)]
struct ServerStream {
    pending: Vec<u8>,
    pending_data_len: Option<usize>,
}

impl ServerStream {
    fn push(&mut self, bytes: &[u8]) -> Vec<ServerEvent> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        self.drain(&mut events);
        events
    }

    fn drain(&mut self, events: &mut Vec<ServerEvent>) {
        loop {
            if let Some(len) = self.pending_data_len {
                if self.pending.len() < len {
                    return;
                }
                if len > 0 {
                    events.push(ServerEvent::Output(self.pending.drain(..len).collect()));
                }
                self.pending_data_len = None;
                continue;
            }
            if self.pending.is_empty() {
                return;
            }
            let Some(start) = find_subslice(&self.pending, CONTROL_PREFIX) else {
                let keep = prefix_overlap(&self.pending, CONTROL_PREFIX);
                let output_len = self.pending.len().saturating_sub(keep);
                if output_len > 0 {
                    events.push(ServerEvent::Output(
                        self.pending.drain(..output_len).collect(),
                    ));
                }
                return;
            };
            if start > 0 {
                events.push(ServerEvent::Output(self.pending.drain(..start).collect()));
                continue;
            }

            let suffix_search_start = CONTROL_PREFIX.len();
            let Some(end_rel) = find_subslice(&self.pending[suffix_search_start..], CONTROL_SUFFIX)
            else {
                if self.pending.len() > MAX_CONTROL_BYTES {
                    events.push(ServerEvent::Output(self.pending.drain(..1).collect()));
                    continue;
                }
                return;
            };
            let payload_start = CONTROL_PREFIX.len();
            let payload_end = suffix_search_start + end_rel;
            let frame_end = payload_end + CONTROL_SUFFIX.len();
            let payload = self.pending[payload_start..payload_end].to_vec();
            let frame = self.pending.drain(..frame_end).collect::<Vec<_>>();
            match parse_server_control(&payload) {
                Some(ServerControl::Size(size)) => events.push(ServerEvent::Size(size)),
                Some(ServerControl::Data(len)) => {
                    self.pending_data_len = Some(len);
                }
                None => events.push(ServerEvent::Output(frame)),
            }
        }
    }
}

enum ServerControl {
    Size(TerminalSize),
    Data(usize),
}

fn parse_server_control(payload: &[u8]) -> Option<ServerControl> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut parts = text.split(';');
    match parts.next()? {
        "size" => {
            let cols = parts.next()?.parse::<u16>().ok()?;
            let rows = parts.next()?.parse::<u16>().ok()?;
            if cols == 0 || rows == 0 || parts.next().is_some() {
                return None;
            }
            Some(ServerControl::Size(TerminalSize { cols, rows }))
        }
        "data" => {
            let len = parts.next()?.parse::<usize>().ok()?;
            if len > MAX_DATA_FRAME_BYTES || parts.next().is_some() {
                return None;
            }
            Some(ServerControl::Data(len))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalSize {
    cols: u16,
    rows: u16,
}

fn send_resize_if_changed(
    stream_fd: RawFd,
    stdout_fd: RawFd,
    last_size: &mut Option<TerminalSize>,
) -> anyhow::Result<()> {
    let Some(size) = terminal_size(stdout_fd) else {
        return Ok(());
    };
    if Some(size) == *last_size {
        return Ok(());
    }
    let frame = encode_resize_control(size);
    write_all(stream_fd, &frame)?;
    *last_size = Some(size);
    Ok(())
}

fn terminal_size(fd: RawFd) -> Option<TerminalSize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) };
    if rc == 0 && size.ws_col > 0 && size.ws_row > 0 {
        Some(TerminalSize {
            cols: size.ws_col,
            rows: size.ws_row,
        })
    } else {
        None
    }
}

fn encode_resize_control(size: TerminalSize) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(CONTROL_PREFIX);
    frame.extend_from_slice(format!("resize;{};{}", size.cols, size.rows).as_bytes());
    frame.extend_from_slice(CONTROL_SUFFIX);
    frame
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn prefix_overlap(haystack: &[u8], prefix: &[u8]) -> usize {
    let max = haystack.len().min(prefix.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|&len| haystack[haystack.len() - len..] == prefix[..len])
        .unwrap_or(0)
}

fn write_all(fd: RawFd, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("ptyconnect write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("ptyconnect write failed: {err}")),
        }
    }
    Ok(())
}

struct RawModeGuard {
    fd: RawFd,
    original: Termios,
}

impl RawModeGuard {
    fn enter(fd: RawFd) -> anyhow::Result<Self> {
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

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;
    use std::thread;

    use clap::{CommandFactory, Parser};
    use nix::unistd::{pipe, read as nix_read, write as nix_write};

    use super::{
        Args, ServerEvent, ServerStream, TerminalSize, encode_resize_control, relay_fds,
        render_viewport,
    };

    #[test]
    fn parses_session_addr() {
        let args = Args::try_parse_from(["ptyconnect", "127.0.0.1:7000"]).unwrap();

        assert_eq!(args.addr.port(), 7000);
    }

    #[test]
    fn help_warns_about_transport_security() {
        let help = Args::command().render_long_help().to_string();

        assert!(help.contains("no built-in auth"));
        assert!(help.contains("trusted tunnel"));
    }

    #[test]
    fn relay_continues_reading_socket_after_stdin_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut input = Vec::new();
            socket.read_to_end(&mut input).unwrap();
            assert_eq!(input, b"ping");
            socket.write_all(b"pong").unwrap();
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
    fn resize_control_is_dcs_framed() {
        assert_eq!(
            encode_resize_control(TerminalSize {
                cols: 100,
                rows: 30
            }),
            b"\x1bPptyshare;resize;100;30\x1b\\"
        );
    }

    #[test]
    fn server_size_control_is_filtered_from_output() {
        let mut stream = ServerStream::default();

        assert_eq!(
            stream.push(b"before\x1bPptyshare;size;40;10\x1b\\after"),
            vec![
                ServerEvent::Output(b"before".to_vec()),
                ServerEvent::Size(TerminalSize { cols: 40, rows: 10 }),
                ServerEvent::Output(b"after".to_vec()),
            ]
        );
    }

    #[test]
    fn server_control_parser_handles_split_frames() {
        let mut stream = ServerStream::default();

        assert_eq!(
            stream.push(b"hello\x1bPpty"),
            vec![ServerEvent::Output(b"hello".to_vec()),]
        );
        assert_eq!(stream.push(b"share;size;80;24"), Vec::new());
        assert_eq!(
            stream.push(b"\x1b\\world"),
            vec![
                ServerEvent::Size(TerminalSize { cols: 80, rows: 24 }),
                ServerEvent::Output(b"world".to_vec()),
            ]
        );
    }

    #[test]
    fn server_data_frame_emits_control_lookalike_bytes_as_output() {
        let mut stream = ServerStream::default();
        let payload = b"before\x1bPptyshare;size;1;1\x1b\\after";
        let frame = server_data_frame(payload);

        assert_eq!(
            stream.push(&frame),
            vec![ServerEvent::Output(payload.to_vec())]
        );
    }

    #[test]
    fn server_data_frame_handles_split_payload() {
        let mut stream = ServerStream::default();
        let mut frame = server_data_frame(b"abcdef");
        let tail = frame.split_off(frame.len() - 2);

        assert_eq!(stream.push(&frame), Vec::new());
        assert_eq!(
            stream.push(&tail),
            vec![ServerEvent::Output(b"abcdef".to_vec())]
        );
    }

    #[test]
    fn viewport_renderer_clips_to_local_terminal_size() {
        let mut parser = vt100::Parser::new(2, 5, 0);
        parser.process(b"hello\r\nworld");

        let rendered = render_viewport(parser.screen(), Some(TerminalSize { cols: 3, rows: 1 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("hel"));
        assert!(!text.contains("world"));
    }

    fn server_data_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(b"\x1bPptyshare;");
        frame.extend_from_slice(format!("data;{}", payload.len()).as_bytes());
        frame.extend_from_slice(b"\x1b\\");
        frame.extend_from_slice(payload);
        frame
    }
}
