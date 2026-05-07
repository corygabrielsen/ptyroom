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
use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{read, write};

use super::process;
use crate::recording::{DwellMs, TraceBuilder};

const MAX_CLIENT_BACKLOG_BYTES: usize = 1024 * 1024;
const MAX_JOIN_REPLAY_BYTES: usize = 256 * 1024;
const MAX_CONTROL_BYTES: usize = 1024;
const SIZE_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const CONTROL_PREFIX: &[u8] = b"\x1bPptyshare;";
const CONTROL_SUFFIX: &[u8] = b"\x1b\\";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalSize {
    cols: u16,
    rows: u16,
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
    let stdout = std::io::stdout();
    let stdout_fd = stdout.as_raw_fd();
    let mut local_stdin_open = opts.local_input;
    let _raw_mode = if opts.local_input && stdin.is_terminal() {
        RawModeGuard::enter(stdin_fd).ok()
    } else {
        None
    };
    let initial_size = TerminalSize {
        cols: opts.cols,
        rows: opts.rows,
    };
    let mut current_size = initial_size;
    let mut host_size = if opts.local_output && stdout.is_terminal() {
        terminal_size(stdout_fd)
    } else {
        None
    };
    let mut last_size_check = Instant::now();
    let mut clients: Vec<Client> = Vec::new();
    let mut join_replay = VecDeque::new();
    let mut stats = ShareStats::default();
    let mut builder = TraceBuilder::new();
    let started = Instant::now();
    let mut last_event = started;
    let mut buf = [0_u8; 4096];

    loop {
        if started.elapsed() > opts.max_runtime {
            break;
        }
        if opts.local_output && last_size_check.elapsed() >= SIZE_CHECK_INTERVAL {
            host_size = terminal_size(stdout_fd);
            last_size_check = Instant::now();
        }

        let poll_state = poll_share_fds(
            listener_fd,
            pty_fd,
            local_stdin_open.then_some(stdin_fd),
            &clients,
        )?;

        process_client_revents(pty_fd, &mut clients, &poll_state.client_revents, &mut stats)?;

        if poll_state.listener_readable {
            stats.accepted +=
                accept_ready_clients(listener, &mut clients, &join_replay, current_size)?;
        }

        if let Some(size) = sync_pty_size(
            &mut pty,
            &mut current_size,
            initial_size,
            host_size,
            &clients,
        )? {
            record_resize_event(&mut builder, &mut last_event, size)?;
            broadcast_control(&mut clients, &encode_size_control(size), &mut stats);
        }

        local_stdin_open = maybe_drain_local_input(
            poll_state.stdin_revents,
            local_stdin_open,
            stdin_fd,
            pty_fd,
            &mut buf,
        )?;

        if !handle_pty_revents(
            poll_state.pty_revents,
            pty_fd,
            &mut buf,
            PtyOutputSinks {
                local_output: opts.local_output,
                stdout_fd,
                join_replay: &mut join_replay,
                clients: &mut clients,
                stats: &mut stats,
            },
            &mut builder,
            &mut last_event,
        )? {
            break;
        }
    }

    finish_share_run(
        &mut pty,
        builder,
        initial_size,
        opts.out,
        listen_addr,
        &stats,
    )
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
    join_replay: &VecDeque<u8>,
    current_size: TerminalSize,
) -> anyhow::Result<usize> {
    let mut accepted = 0;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let mut client = Client::new(stream)?;
                client.enqueue(&encode_size_control(current_size));
                client.enqueue_deque(join_replay);
                clients.push(client);
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

fn maybe_drain_local_input(
    revents: PollFlags,
    open: bool,
    stdin_fd: i32,
    pty_fd: i32,
    buf: &mut [u8],
) -> anyhow::Result<bool> {
    if open && revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
        drain_local_input(stdin_fd, pty_fd, buf)
    } else {
        Ok(open)
    }
}

fn sync_pty_size(
    pty: &mut process::PtyMaster,
    current: &mut TerminalSize,
    fallback: TerminalSize,
    host_size: Option<TerminalSize>,
    clients: &[Client],
) -> anyhow::Result<Option<TerminalSize>> {
    let desired = desired_session_size(fallback, host_size, clients);
    if desired == *current {
        return Ok(None);
    }
    pty.resize(desired.cols, desired.rows)?;
    *current = desired;
    Ok(Some(desired))
}

fn desired_session_size(
    fallback: TerminalSize,
    host_size: Option<TerminalSize>,
    clients: &[Client],
) -> TerminalSize {
    let mut sizes = host_size
        .into_iter()
        .chain(clients.iter().filter_map(|client| client.size));
    let Some(mut desired) = sizes.next() else {
        return fallback;
    };
    for size in sizes {
        desired.cols = desired.cols.min(size.cols);
        desired.rows = desired.rows.min(size.rows);
    }
    desired
}

fn terminal_size(fd: i32) -> Option<TerminalSize> {
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

fn broadcast_control(clients: &mut Vec<Client>, bytes: &[u8], stats: &mut ShareStats) {
    broadcast(clients, bytes, stats);
}

fn encode_size_control(size: TerminalSize) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(CONTROL_PREFIX);
    frame.extend_from_slice(format!("size;{};{}", size.cols, size.rows).as_bytes());
    frame.extend_from_slice(CONTROL_SUFFIX);
    frame
}

fn encode_output_frame(bytes: &[u8]) -> Vec<u8> {
    let mut frame =
        Vec::with_capacity(CONTROL_PREFIX.len() + 24 + CONTROL_SUFFIX.len() + bytes.len());
    frame.extend_from_slice(CONTROL_PREFIX);
    frame.extend_from_slice(format!("data;{}", bytes.len()).as_bytes());
    frame.extend_from_slice(CONTROL_SUFFIX);
    frame.extend_from_slice(bytes);
    frame
}

fn remember_for_late_joiners(replay: &mut VecDeque<u8>, bytes: &[u8]) {
    replay.extend(bytes.iter().copied());
    let excess = replay.len().saturating_sub(MAX_JOIN_REPLAY_BYTES);
    if excess > 0 {
        drop(replay.drain(..excess));
    }
}

#[derive(Debug)]
struct Client {
    stream: TcpStream,
    pending: VecDeque<u8>,
    input_pending: Vec<u8>,
    input_open: bool,
    size: Option<TerminalSize>,
}

impl Client {
    fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            pending: VecDeque::new(),
            input_pending: Vec::new(),
            input_open: true,
            size: None,
        })
    }

    fn poll_flags(&self) -> PollFlags {
        let mut flags = PollFlags::empty();
        if self.input_open {
            flags |= PollFlags::POLLIN;
        }
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

    fn enqueue_deque(&mut self, bytes: &VecDeque<u8>) -> bool {
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
                Ok(0) => {
                    self.flush_pending_input_as_raw(pty_fd)?;
                    return Ok(false);
                }
                Ok(n) => {
                    self.input_pending.extend_from_slice(&buf[..n]);
                    self.flush_pending_input(pty_fd)?;
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }
    }

    fn flush_pending_input(&mut self, pty_fd: i32) -> anyhow::Result<()> {
        loop {
            if self.input_pending.is_empty() {
                return Ok(());
            }
            let Some(start) = find_subslice(&self.input_pending, CONTROL_PREFIX) else {
                let keep = prefix_overlap(&self.input_pending, CONTROL_PREFIX);
                let write_len = self.input_pending.len().saturating_sub(keep);
                if write_len > 0 {
                    write_all(pty_fd, &self.input_pending[..write_len])?;
                    self.input_pending.drain(..write_len);
                }
                return Ok(());
            };
            if start > 0 {
                write_all(pty_fd, &self.input_pending[..start])?;
                self.input_pending.drain(..start);
                continue;
            }

            let suffix_search_start = CONTROL_PREFIX.len();
            let Some(end_rel) =
                find_subslice(&self.input_pending[suffix_search_start..], CONTROL_SUFFIX)
            else {
                if self.input_pending.len() > MAX_CONTROL_BYTES {
                    write_all(pty_fd, &self.input_pending[..1])?;
                    self.input_pending.drain(..1);
                    continue;
                }
                return Ok(());
            };
            let payload_start = CONTROL_PREFIX.len();
            let payload_end = suffix_search_start + end_rel;
            let payload = self.input_pending[payload_start..payload_end].to_vec();
            self.apply_control(&payload);
            self.input_pending
                .drain(..payload_end + CONTROL_SUFFIX.len());
        }
    }

    fn flush_pending_input_as_raw(&mut self, pty_fd: i32) -> anyhow::Result<()> {
        if !self.input_pending.is_empty() {
            write_all(pty_fd, &self.input_pending)?;
            self.input_pending.clear();
        }
        Ok(())
    }

    fn apply_control(&mut self, payload: &[u8]) {
        let Ok(text) = std::str::from_utf8(payload) else {
            return;
        };
        let mut parts = text.split(';');
        if parts.next() != Some("resize") {
            return;
        }
        let Some(cols) = parts.next().and_then(|value| value.parse::<u16>().ok()) else {
            return;
        };
        let Some(rows) = parts.next().and_then(|value| value.parse::<u16>().ok()) else {
            return;
        };
        if cols > 0 && rows > 0 && parts.next().is_none() {
            self.size = Some(TerminalSize { cols, rows });
        }
    }

    fn disconnect(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
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

fn finish_share_run(
    pty: &mut process::PtyMaster,
    builder: TraceBuilder,
    initial_size: TerminalSize,
    out: PathBuf,
    listen_addr: SocketAddr,
    stats: &ShareStats,
) -> anyhow::Result<ShareSummary> {
    pty.terminate_child();
    let (trace_path, events) = finish_share_trace(builder, initial_size, out)?;
    Ok(ShareSummary {
        listen_addr,
        trace_path,
        events,
        clients_accepted: stats.accepted,
        clients_disconnected: stats.disconnected,
        clients_dropped_for_backlog: stats.dropped_for_backlog,
    })
}

struct PtyOutputSinks<'a> {
    local_output: bool,
    stdout_fd: i32,
    join_replay: &'a mut VecDeque<u8>,
    clients: &'a mut Vec<Client>,
    stats: &'a mut ShareStats,
}

fn handle_pty_revents(
    revents: PollFlags,
    pty_fd: i32,
    buf: &mut [u8],
    sinks: PtyOutputSinks<'_>,
    builder: &mut TraceBuilder,
    last_event: &mut Instant,
) -> anyhow::Result<bool> {
    if revents.intersects(PollFlags::POLLIN) {
        let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        match read(pty_borrow, buf) {
            Ok(0) | Err(Errno::EIO) => return Ok(false),
            Ok(n) => handle_pty_output(&buf[..n], sinks, builder, last_event)?,
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
    last_event: &mut Instant,
) -> anyhow::Result<()> {
    let PtyOutputSinks {
        local_output,
        stdout_fd,
        join_replay,
        clients,
        stats,
    } = sinks;
    if local_output {
        let stdout_borrow = unsafe { BorrowedFd::borrow_raw(stdout_fd) };
        let _ = write(stdout_borrow, bytes);
    }
    let client_frame = encode_output_frame(bytes);
    remember_for_late_joiners(join_replay, &client_frame);
    broadcast(clients, &client_frame, stats);
    let now = Instant::now();
    let dwell = DwellMs::from_duration(now.saturating_duration_since(*last_event));
    builder.record_output(bytes.to_vec(), dwell)?;
    *last_event = now;
    Ok(())
}

fn record_resize_event(
    builder: &mut TraceBuilder,
    last_event: &mut Instant,
    size: TerminalSize,
) -> anyhow::Result<()> {
    let now = Instant::now();
    let dwell = DwellMs::from_duration(now.saturating_duration_since(*last_event));
    builder.record_resize(size.cols, size.rows, dwell)?;
    *last_event = now;
    Ok(())
}

fn finish_share_trace(
    builder: TraceBuilder,
    size: TerminalSize,
    out: PathBuf,
) -> anyhow::Result<(PathBuf, usize)> {
    let recording = builder.finish_screen(size.cols, size.rows)?;
    let trace = recording.into_trace();
    let events = trace.events.len();
    write_trace(&trace, &out)?;
    Ok((out, events))
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
    use crate::trace::{EventKind, Trace};

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
    fn half_closed_client_still_receives_resulting_output() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared-half-close.ptytrace");
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
                            "read line; printf 'half:%s\\n' \"$line\"".into(),
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

        let mut client = connect_with_retry(addr);
        client.write_all(b"hello\n").unwrap();
        client.shutdown(Shutdown::Write).unwrap();

        assert_contains_from_stream(&mut client, "half:hello");
        let summary = handle.join().unwrap().unwrap();
        assert_eq!(summary.clients_accepted, 1);
        assert_eq!(summary.clients_dropped_for_backlog, 0);
    }

    #[test]
    fn late_joining_client_receives_recent_output_before_typing() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared-late-join.ptytrace");
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
                            "sleep 0.2; printf 'ready\\n'; read line; printf 'late:%s\\n' \"$line\""
                                .into(),
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

        let mut early = connect_with_retry(addr);
        assert_contains_from_stream(&mut early, "ready");
        drop(early);

        let mut late = connect_with_retry(addr);
        assert_contains_from_stream(&mut late, "ready");
        late.write_all(b"hello\n").unwrap();
        assert_contains_from_stream(&mut late, "late:hello");

        let summary = handle.join().unwrap().unwrap();
        assert_eq!(summary.clients_accepted, 2);
        assert_eq!(summary.clients_dropped_for_backlog, 0);
    }

    #[test]
    fn resize_control_updates_child_pty_size_without_reaching_input() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared-resize.ptytrace");
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
                            "sleep 0.2; stty size; read line; printf 'line:%s\\n' \"$line\"".into(),
                        ],
                        cols: 100,
                        rows: 30,
                        out,
                        local_output: false,
                        local_input: false,
                        max_runtime: Duration::from_secs(5),
                    },
                )
            }
        });

        let mut client = connect_with_retry(addr);
        client.write_all(resize_control(40, 10).as_slice()).unwrap();
        assert_contains_from_stream(&mut client, "10 40");
        client.write_all(b"hello\n").unwrap();
        assert_contains_from_stream(&mut client, "line:hello");

        let summary = handle.join().unwrap().unwrap();
        assert_eq!(summary.clients_accepted, 1);
        assert_eq!(summary.clients_dropped_for_backlog, 0);
        let trace = Trace::read(summary.trace_path).unwrap();
        assert_eq!(trace.header.width, 100);
        assert_eq!(trace.header.height, 30);
        assert!(
            trace
                .events
                .iter()
                .any(|event| { matches!(event.kind, EventKind::Resize) && event.data == "40x10" })
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

    #[test]
    fn desired_session_size_recomputes_from_active_participants() {
        let fallback = TerminalSize {
            cols: 100,
            rows: 30,
        };
        let small = client_with_size(TerminalSize { cols: 40, rows: 10 });
        let large = client_with_size(TerminalSize { cols: 90, rows: 25 });

        assert_eq!(
            desired_session_size(fallback, None, &[small, large]),
            TerminalSize { cols: 40, rows: 10 }
        );

        let large = client_with_size(TerminalSize { cols: 90, rows: 25 });
        assert_eq!(
            desired_session_size(fallback, None, &[large]),
            TerminalSize { cols: 90, rows: 25 }
        );
        assert_eq!(desired_session_size(fallback, None, &[]), fallback);
    }

    #[test]
    fn output_frames_preserve_control_lookalikes_as_data() {
        let payload = b"before\x1bPptyshare;size;1;1\x1b\\after";
        let mut expected = Vec::new();
        expected.extend_from_slice(CONTROL_PREFIX);
        expected.extend_from_slice(format!("data;{}", payload.len()).as_bytes());
        expected.extend_from_slice(CONTROL_SUFFIX);
        expected.extend_from_slice(payload);

        assert_eq!(encode_output_frame(payload), expected);
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

    fn resize_control(cols: u16, rows: u16) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(CONTROL_PREFIX);
        frame.extend_from_slice(format!("resize;{cols};{rows}").as_bytes());
        frame.extend_from_slice(CONTROL_SUFFIX);
        frame
    }

    fn client_with_size(size: TerminalSize) -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        client.size = Some(size);
        client
    }
}
