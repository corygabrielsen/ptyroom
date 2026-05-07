//! Shared PTY sessions over TCP.
//!
//! This is the first network primitive for collaborative terminal
//! sessions: one host process owns the PTY, clients connect over TCP,
//! client input bytes are interleaved into the PTY, and PTY output is
//! broadcast back to every client while being recorded as a trace.
//!
//! Security boundary: this module provides transport plumbing, not
//! authentication or encryption. Bind to loopback by default and put
//! SSH, `WireGuard`, or another authenticated tunnel in front when the
//! session crosses a machine boundary.

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{read, write};

use super::process;
use crate::recording::{DwellMs, TraceBuilder};

const MAX_CLIENT_BACKLOG_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ShareOpts {
    /// Command to run under the shared PTY. Empty uses `$SHELL` or `bash`.
    pub argv: Vec<String>,
    /// Terminal columns.
    pub cols: u16,
    /// Terminal rows.
    pub rows: u16,
    /// Output trace path.
    pub out: PathBuf,
    /// Maximum wall-clock session duration.
    pub max_runtime: Duration,
    /// Also tee PTY output to the share host's stdout.
    pub local_output: bool,
    /// Also forward the share host's stdin into the PTY.
    pub local_input: bool,
}

impl Default for ShareOpts {
    fn default() -> Self {
        Self {
            argv: Vec::new(),
            cols: 80,
            rows: 24,
            out: PathBuf::from("shared.ptytrace"),
            max_runtime: Duration::from_hours(1),
            local_output: true,
            local_input: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSummary {
    pub listen_addr: SocketAddr,
    pub trace_path: PathBuf,
    pub events: usize,
    pub clients_accepted: usize,
    pub clients_disconnected: usize,
    pub clients_dropped_for_backlog: usize,
}

/// Run a shared PTY session using an already-bound listener.
///
/// # Errors
/// PTY spawn, listener, client IO, trace construction, or trace write
/// failed.
pub fn run(listener: &TcpListener, opts: ShareOpts) -> anyhow::Result<ShareSummary> {
    listener.set_nonblocking(true)?;
    let listen_addr = listener.local_addr()?;
    let argv = resolve_argv(opts.argv);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut pty = process::spawn(&argv_refs, opts.cols, opts.rows)?;
    let pty_fd = pty.fd();
    let listener_fd = listener.as_raw_fd();
    let stdin = io::stdin();
    let stdin_fd = stdin.as_raw_fd();
    let stdout_fd = std::io::stdout().as_raw_fd();
    let mut local_stdin_open = opts.local_input;
    let _raw_mode = if opts.local_input && stdin.is_terminal() {
        RawModeGuard::enter(stdin_fd).ok()
    } else {
        None
    };
    let mut clients: Vec<Client> = Vec::new();
    let mut stats = ShareStats::default();
    let mut builder = TraceBuilder::new();
    let started = Instant::now();
    let mut last_event = started;
    let mut buf = [0_u8; 4096];

    loop {
        if started.elapsed() > opts.max_runtime {
            break;
        }

        let poll_state = poll_share_fds(
            listener_fd,
            pty_fd,
            local_stdin_open.then_some(stdin_fd),
            &clients,
        )?;

        process_client_revents(pty_fd, &mut clients, &poll_state.client_revents, &mut stats)?;

        if poll_state.listener_readable {
            stats.accepted += accept_ready_clients(listener, &mut clients)?;
        }

        if poll_state
            .stdin_revents
            .intersects(PollFlags::POLLIN | PollFlags::POLLHUP)
        {
            local_stdin_open = drain_local_input(stdin_fd, pty_fd, &mut buf)?;
        }

        if poll_state.pty_revents.intersects(PollFlags::POLLIN) {
            let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
            match read(pty_borrow, &mut buf) {
                Ok(0) | Err(Errno::EIO) => break,
                Ok(n) => {
                    let bytes = &buf[..n];
                    if opts.local_output {
                        let stdout_borrow = unsafe { BorrowedFd::borrow_raw(stdout_fd) };
                        let _ = write(stdout_borrow, bytes);
                    }
                    broadcast(&mut clients, bytes, &mut stats);
                    let now = Instant::now();
                    let dwell = DwellMs::from_duration(now.saturating_duration_since(last_event));
                    builder.record_output(bytes.to_vec(), dwell)?;
                    last_event = now;
                }
                Err(Errno::EINTR) => {}
                Err(err) => return Err(anyhow!("read shared PTY: {err}")),
            }
        }
        if poll_state
            .pty_revents
            .intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL)
        {
            break;
        }
    }

    pty.terminate_child();
    let recording = builder.finish_screen(opts.cols, opts.rows)?;
    let trace = recording.into_trace();
    let events = trace.events.len();
    write_trace(&trace, &opts.out)?;
    Ok(ShareSummary {
        listen_addr,
        trace_path: opts.out,
        events,
        clients_accepted: stats.accepted,
        clients_disconnected: stats.disconnected,
        clients_dropped_for_backlog: stats.dropped_for_backlog,
    })
}

#[derive(Debug, Default)]
struct ShareStats {
    accepted: usize,
    disconnected: usize,
    dropped_for_backlog: usize,
}

#[derive(Debug)]
struct PollState {
    listener_readable: bool,
    pty_revents: PollFlags,
    stdin_revents: PollFlags,
    client_revents: Vec<PollFlags>,
}

fn poll_share_fds(
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
        let client_borrow = unsafe { BorrowedFd::borrow_raw(client.stream.as_raw_fd()) };
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

fn accept_ready_clients(
    listener: &TcpListener,
    clients: &mut Vec<Client>,
) -> anyhow::Result<usize> {
    let mut accepted = 0;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                clients.push(Client::new(stream)?);
                accepted += 1;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(accepted),
            Err(err) => return Err(err).context("accept ptyshare client"),
        }
    }
}

fn process_client_revents(
    pty_fd: i32,
    clients: &mut Vec<Client>,
    revents: &[PollFlags],
    stats: &mut ShareStats,
) -> anyhow::Result<()> {
    let mut kept = Vec::with_capacity(clients.len());
    for (idx, mut client) in clients.drain(..).enumerate() {
        let rev = revents.get(idx).copied().unwrap_or_else(PollFlags::empty);
        let mut keep =
            !rev.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL);
        if keep && rev.intersects(PollFlags::POLLIN) {
            keep = client.drain_input_to_pty(pty_fd)?;
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

fn drain_local_input(stdin_fd: i32, pty_fd: i32, buf: &mut [u8]) -> anyhow::Result<bool> {
    let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
    match read(stdin_borrow, buf) {
        Ok(0) | Err(Errno::EIO) => Ok(false),
        Ok(n) => {
            write_all(pty_fd, &buf[..n])?;
            Ok(true)
        }
        Err(Errno::EINTR) => Ok(true),
        Err(err) => Err(anyhow!("read local stdin: {err}")),
    }
}

fn broadcast(clients: &mut Vec<Client>, bytes: &[u8], stats: &mut ShareStats) {
    let mut kept = Vec::with_capacity(clients.len());
    for mut client in clients.drain(..) {
        if !client.enqueue(bytes) {
            client.disconnect();
            stats.disconnected += 1;
            stats.dropped_for_backlog += 1;
        } else if client.flush_pending() {
            kept.push(client);
        } else {
            client.disconnect();
            stats.disconnected += 1;
        }
    }
    *clients = kept;
}

#[derive(Debug)]
struct Client {
    stream: TcpStream,
    pending: VecDeque<u8>,
}

impl Client {
    fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            pending: VecDeque::new(),
        })
    }

    fn poll_flags(&self) -> PollFlags {
        let mut flags = PollFlags::POLLIN;
        if self.has_pending() {
            flags |= PollFlags::POLLOUT;
        }
        flags
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn enqueue(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > MAX_CLIENT_BACKLOG_BYTES.saturating_sub(self.pending.len()) {
            return false;
        }
        self.pending.extend(bytes.iter().copied());
        true
    }

    fn flush_pending(&mut self) -> bool {
        while !self.pending.is_empty() {
            let (front, back) = self.pending.as_slices();
            let chunk = if front.is_empty() { back } else { front };
            if chunk.is_empty() {
                return true;
            }
            match self.stream.write(chunk) {
                Ok(0) => return false,
                Ok(n) => {
                    drop(self.pending.drain(..n));
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return true,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }

    fn drain_input_to_pty(&mut self, pty_fd: i32) -> anyhow::Result<bool> {
        let mut buf = [0_u8; 4096];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => return Ok(false),
                Ok(n) => write_all(pty_fd, &buf[..n])?,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }
    }

    fn disconnect(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

struct RawModeGuard {
    fd: i32,
    original: Termios,
}

impl RawModeGuard {
    fn enter(fd: i32) -> anyhow::Result<Self> {
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

fn write_all(fd: i32, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("pty share write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("write shared input to PTY: {err}")),
        }
    }
    Ok(())
}

fn write_trace(trace: &crate::trace::Trace, path: &Path) -> anyhow::Result<()> {
    trace.write(path)?;
    Ok(())
}

fn resolve_argv(argv: Vec<String>) -> Vec<String> {
    if !argv.is_empty() {
        return argv;
    }
    if let Ok(shell) = std::env::var("SHELL")
        && !shell.is_empty()
    {
        return vec![shell];
    }
    vec!["bash".into()]
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;
    use crate::trace::Trace;

    #[test]
    fn default_share_opts_bind_a_trace_name() {
        assert_eq!(ShareOpts::default().out, PathBuf::from("shared.ptytrace"));
        assert!(ShareOpts::default().local_input);
    }

    #[test]
    fn share_records_command_output_without_clients() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared.ptytrace");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let summary = run(
            &listener,
            ShareOpts {
                argv: vec!["sh".into(), "-lc".into(), "printf ready".into()],
                out: out.clone(),
                local_output: false,
                max_runtime: Duration::from_secs(5),
                ..ShareOpts::default()
            },
        )
        .unwrap();

        assert_eq!(summary.trace_path, out);
        assert!(summary.events > 0);
        let trace = Trace::read(summary.trace_path).unwrap();
        assert!(
            trace
                .events
                .iter()
                .any(|event| event.data.contains("ready"))
        );
    }

    #[test]
    fn share_interleaves_client_input_into_pty() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared-input.ptytrace");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn({
            let out = out.clone();
            move || {
                run(
                    &listener,
                    ShareOpts {
                        argv: vec![
                            "sh".into(),
                            "-lc".into(),
                            "read line; printf 'got:%s\\n' \"$line\"".into(),
                        ],
                        out,
                        local_output: false,
                        max_runtime: Duration::from_secs(5),
                        ..ShareOpts::default()
                    },
                )
            }
        });

        let mut client = connect_with_retry(addr);
        client.write_all(b"hello\n").unwrap();
        let summary = handle.join().unwrap().unwrap();
        assert_eq!(summary.clients_accepted, 1);
        let trace = Trace::read(summary.trace_path).unwrap();
        assert!(
            trace
                .events
                .iter()
                .any(|event| event.data.contains("got:hello"))
        );
    }

    #[test]
    fn share_broadcasts_client_driven_output_to_all_clients() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared-broadcast.ptytrace");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn({
            let out = out.clone();
            move || {
                run(
                    &listener,
                    ShareOpts {
                        argv: vec![
                            "sh".into(),
                            "-lc".into(),
                            "read line; printf 'broadcast:%s\\n' \"$line\"".into(),
                        ],
                        out,
                        local_output: false,
                        local_input: false,
                        max_runtime: Duration::from_secs(5),
                        ..ShareOpts::default()
                    },
                )
            }
        });

        let mut writer = connect_with_retry(addr);
        let mut observer = connect_with_retry(addr);
        writer.write_all(b"hello\n").unwrap();

        assert_contains_from_stream(&mut writer, "broadcast:hello");
        assert_contains_from_stream(&mut observer, "broadcast:hello");
        let summary = handle.join().unwrap().unwrap();
        assert_eq!(summary.clients_accepted, 2);
        assert_eq!(summary.clients_dropped_for_backlog, 0);
    }

    #[test]
    fn disconnected_client_does_not_stop_remaining_client() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared-disconnect.ptytrace");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn({
            let out = out.clone();
            move || {
                run(
                    &listener,
                    ShareOpts {
                        argv: vec![
                            "sh".into(),
                            "-lc".into(),
                            "read first; printf 'first:%s\\n' \"$first\"; read second; printf 'second:%s\\n' \"$second\"".into(),
                        ],
                        out,
                        local_output: false,
                        local_input: false,
                        max_runtime: Duration::from_secs(5),
                        ..ShareOpts::default()
                    },
                )
            }
        });

        let mut transient = connect_with_retry(addr);
        let mut survivor = connect_with_retry(addr);
        transient.write_all(b"alpha\n").unwrap();
        assert_contains_from_stream(&mut survivor, "first:alpha");
        drop(transient);

        survivor.write_all(b"omega\n").unwrap();
        assert_contains_from_stream(&mut survivor, "second:omega");
        let summary = handle.join().unwrap().unwrap();
        assert_eq!(summary.clients_accepted, 2);
        assert!(summary.clients_disconnected >= 1);
        assert_eq!(summary.clients_dropped_for_backlog, 0);
    }

    #[test]
    fn broadcast_drops_clients_that_exceed_backlog_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _peer = TcpStream::connect(addr).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        assert!(client.enqueue(&vec![b'x'; MAX_CLIENT_BACKLOG_BYTES]));
        let mut clients = vec![client];
        let mut stats = ShareStats::default();

        broadcast(&mut clients, b"y", &mut stats);

        assert!(clients.is_empty());
        assert_eq!(stats.disconnected, 1);
        assert_eq!(stats.dropped_for_backlog, 1);
    }

    fn connect_with_retry(addr: SocketAddr) -> TcpStream {
        let started = Instant::now();
        loop {
            match TcpStream::connect(addr) {
                Ok(stream) => return stream,
                Err(err) if started.elapsed() < Duration::from_secs(2) => {
                    assert!(
                        err.kind() == std::io::ErrorKind::ConnectionRefused
                            || err.kind() == std::io::ErrorKind::TimedOut
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("connect to ptyshare test server failed: {err}"),
            }
        }
    }

    fn assert_contains_from_stream(stream: &mut TcpStream, needle: &str) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0_u8; 256];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => panic!("stream closed before seeing {needle:?}"),
                Ok(n) => {
                    bytes.extend_from_slice(&buf[..n]);
                    if String::from_utf8_lossy(&bytes).contains(needle) {
                        return;
                    }
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    panic!(
                        "timed out waiting for {needle:?}; saw {:?}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
                Err(err) => panic!("read from ptyshare client stream failed: {err}"),
            }
        }
    }
}
