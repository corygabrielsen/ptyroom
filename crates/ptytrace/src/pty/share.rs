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
use super::room_protocol::{self, ClientControl, TerminalSize};
use super::status_bar::{Bar, Chip};
use super::terminal_state::{RestoreGuard, child_output_restore_sequence, termination_requested};
use super::viewport::ViewportRenderer;
use crate::recording::{DwellMs, TraceBuilder};

const MAX_CLIENT_BACKLOG_BYTES: usize = 1024 * 1024;
const MAX_JOIN_REPLAY_BYTES: usize = 256 * 1024;
const SIZE_CHECK_INTERVAL: Duration = Duration::from_millis(250);

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

#[must_use]
pub const fn host_local_io_notice(local_input: bool, local_output: bool) -> Option<&'static str> {
    match (local_input, local_output) {
        (false, false) => {
            Some("[host input/output disabled; session is controlled by connected clients]")
        }
        (false, true) => Some(
            "[warning: host input disabled; type from a connected client or remove --no-local-input]",
        ),
        _ => None,
    }
}

/// Run a shared PTY session using an already-bound listener.
///
/// # Errors
/// PTY spawn, listener, client IO, trace construction, or trace write
/// failed.
pub fn run(listener: &TcpListener, opts: ShareOpts) -> anyhow::Result<ShareSummary> {
    let mut session = Session::start(listener, opts)?;
    let mut buf = [0_u8; 4096];
    while !session.should_stop() {
        if !session.tick(&mut buf)? {
            break;
        }
    }
    session.finish()
}

struct Session<'a> {
    listener: &'a TcpListener,
    pty: process::PtyMaster,
    listener_fd: i32,
    pty_fd: i32,
    stdin_fd: i32,
    stdout_fd: i32,
    local_output: bool,
    initial_size: TerminalSize,
    current_size: TerminalSize,
    host_size: Option<TerminalSize>,
    last_size_check: Instant,
    host_viewport: Option<HostViewport>,
    local_stdin_open: bool,
    clients: Vec<Client>,
    join_replay: JoinReplay,
    stats: ShareStats,
    builder: TraceBuilder,
    last_event: Instant,
    listen_addr: SocketAddr,
    out_path: PathBuf,
    started: Instant,
    max_runtime: Duration,
    _terminal_cleanup: Option<RestoreGuard>,
    _raw_mode: Option<RawModeGuard>,
}

impl<'a> Session<'a> {
    fn start(listener: &'a TcpListener, opts: ShareOpts) -> anyhow::Result<Self> {
        ensure_nonzero_size(opts.cols, opts.rows)?;
        listener.set_nonblocking(true)?;
        let listen_addr = listener.local_addr()?;
        let argv = resolve_argv(opts.argv);
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let pty = process::spawn(&argv_refs, opts.cols, opts.rows)?;
        let pty_fd = pty.fd();
        let listener_fd = listener.as_raw_fd();
        let stdin = io::stdin();
        let stdin_fd = stdin.as_raw_fd();
        let stdout = std::io::stdout();
        let stdout_fd = stdout.as_raw_fd();
        let (host_viewport, terminal_cleanup) =
            setup_host_terminal(opts.local_output, &argv, listen_addr, &stdout, stdout_fd)?;
        let raw_mode = host_raw_mode_guard(opts.local_input, &stdin, stdin_fd)?;
        let initial_size = TerminalSize::new(opts.cols, opts.rows);
        let host_size = initial_host_size(
            opts.local_output,
            &stdout,
            stdout_fd,
            host_viewport.as_ref(),
        );
        let started = Instant::now();
        Ok(Self {
            listener,
            pty,
            listener_fd,
            pty_fd,
            stdin_fd,
            stdout_fd,
            local_output: opts.local_output,
            initial_size,
            current_size: initial_size,
            host_size,
            last_size_check: Instant::now(),
            host_viewport,
            local_stdin_open: opts.local_input,
            clients: Vec::new(),
            join_replay: JoinReplay::default(),
            stats: ShareStats::default(),
            builder: TraceBuilder::new(),
            last_event: started,
            listen_addr,
            out_path: opts.out,
            started,
            max_runtime: opts.max_runtime,
            _terminal_cleanup: terminal_cleanup,
            _raw_mode: raw_mode,
        })
    }

    fn should_stop(&self) -> bool {
        termination_requested() || self.started.elapsed() > self.max_runtime
    }

    fn tick(&mut self, buf: &mut [u8]) -> anyhow::Result<bool> {
        refresh_host_size(
            self.local_output,
            self.host_viewport.is_some(),
            self.stdout_fd,
            &mut self.host_size,
            &mut self.last_size_check,
        );
        let poll_state = poll_share_fds(
            self.listener_fd,
            self.pty_fd,
            self.local_stdin_open.then_some(self.stdin_fd),
            &self.clients,
        )?;
        process_client_revents(
            self.pty_fd,
            &mut self.clients,
            &poll_state.client_revents,
            &mut self.stats,
        )?;
        if poll_state.listener_readable {
            self.stats.accepted += accept_ready_clients(
                self.listener,
                &mut self.clients,
                &self.join_replay,
                self.current_size,
            )?;
        }
        if let Some(viewport) = self.host_viewport.as_mut() {
            viewport.set_client_count(self.clients.len())?;
        }
        sync_canonical_size(
            &mut self.pty,
            &mut self.current_size,
            self.initial_size,
            self.host_size,
            &mut self.clients,
            self.host_viewport.as_mut(),
            self.stdout_fd,
            &mut self.builder,
            &mut self.last_event,
            &mut self.stats,
        )?;
        self.local_stdin_open = maybe_drain_local_input(
            poll_state.stdin_revents,
            self.local_stdin_open,
            self.stdin_fd,
            self.pty_fd,
            buf,
        )?;
        handle_pty_revents(
            poll_state.pty_revents,
            self.pty_fd,
            buf,
            PtyOutputSinks {
                local_output: self.local_output,
                stdout_fd: self.stdout_fd,
                host_viewport: self.host_viewport.as_mut(),
                join_replay: &mut self.join_replay,
                clients: &mut self.clients,
                stats: &mut self.stats,
            },
            &mut self.builder,
            &mut self.last_event,
        )
    }

    fn finish(mut self) -> anyhow::Result<ShareSummary> {
        finish_share_run(
            &mut self.pty,
            self.builder,
            self.initial_size,
            self.out_path,
            self.listen_addr,
            &self.stats,
        )
    }
}

fn ensure_nonzero_size(cols: u16, rows: u16) -> anyhow::Result<()> {
    if cols == 0 || rows == 0 {
        anyhow::bail!("ptyroom initial terminal size must be nonzero; got {cols}x{rows}");
    }
    Ok(())
}

fn host_raw_mode_guard(
    local_input: bool,
    stdin: &io::Stdin,
    stdin_fd: i32,
) -> anyhow::Result<Option<RawModeGuard>> {
    if local_input && stdin.is_terminal() {
        return Ok(Some(
            RawModeGuard::enter(stdin_fd).context("enter raw mode for ptyroom host stdin")?,
        ));
    }
    Ok(None)
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

fn refresh_host_size(
    local_output: bool,
    viewport_active: bool,
    stdout_fd: i32,
    host_size: &mut Option<TerminalSize>,
    last_size_check: &mut Instant,
) {
    if local_output && last_size_check.elapsed() >= SIZE_CHECK_INTERVAL {
        *host_size = if viewport_active {
            HostViewport::reported_size(stdout_fd)
        } else {
            terminal_size(stdout_fd)
        };
        *last_size_check = Instant::now();
    }
}

fn terminal_cleanup_guard(
    local_output: bool,
    stdout: &io::Stdout,
    fd: i32,
) -> Option<RestoreGuard> {
    if cfg!(test) {
        return None;
    }
    (local_output && stdout.is_terminal())
        .then_some(RestoreGuard::new(fd, child_output_restore_sequence()))
}

fn setup_host_terminal(
    local_output: bool,
    argv: &[String],
    listen_addr: SocketAddr,
    stdout: &io::Stdout,
    stdout_fd: i32,
) -> anyhow::Result<(Option<HostViewport>, Option<RestoreGuard>)> {
    let viewport_enabled = local_output && stdout.is_terminal() && !cfg!(test);
    if viewport_enabled {
        let viewport = HostViewport::enter(stdout_fd, listen_addr.to_string(), argv.join(" "))?;
        Ok((Some(viewport), None))
    } else {
        let cleanup = terminal_cleanup_guard(local_output, stdout, stdout_fd);
        Ok((None, cleanup))
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_canonical_size(
    pty: &mut process::PtyMaster,
    current_size: &mut TerminalSize,
    initial_size: TerminalSize,
    host_size: Option<TerminalSize>,
    clients: &mut Vec<Client>,
    host_viewport: Option<&mut HostViewport>,
    stdout_fd: i32,
    builder: &mut TraceBuilder,
    last_event: &mut Instant,
    stats: &mut ShareStats,
) -> anyhow::Result<()> {
    let Some(size) = sync_pty_size(pty, current_size, initial_size, host_size, clients)? else {
        return Ok(());
    };
    record_resize_event(builder, last_event, size)?;
    broadcast_control(clients, &room_protocol::encode_size_control(size), stats);
    if let Some(viewport) = host_viewport {
        viewport.resize(stdout_fd, size)?;
    }
    Ok(())
}

fn initial_host_size(
    local_output: bool,
    stdout: &io::Stdout,
    stdout_fd: i32,
    host_viewport: Option<&HostViewport>,
) -> Option<TerminalSize> {
    if host_viewport.is_some() {
        HostViewport::reported_size(stdout_fd)
    } else if local_output && stdout.is_terminal() {
        terminal_size(stdout_fd)
    } else {
        None
    }
}

struct HostViewport {
    inner: ViewportRenderer,
    addr: String,
    command: String,
    client_count: usize,
}

impl HostViewport {
    fn enter(stdout_fd: i32, addr: String, command: String) -> anyhow::Result<Self> {
        let bar = build_host_bar(&addr, &command, 0);
        let title = format!("ptyroom host {addr}");
        let inner = ViewportRenderer::enter(stdout_fd, &title, &bar)?;
        Ok(Self {
            inner,
            addr,
            command,
            client_count: 0,
        })
    }

    fn process_output(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.inner.process_output(bytes, &self.bar())
    }

    fn resize(&mut self, stdout_fd: i32, size: TerminalSize) -> anyhow::Result<()> {
        self.inner.resize(stdout_fd, size, &self.bar())
    }

    fn set_client_count(&mut self, count: usize) -> anyhow::Result<()> {
        if self.client_count == count {
            return Ok(());
        }
        self.client_count = count;
        self.inner.redraw_status(&self.bar())
    }

    fn reported_size(stdout_fd: i32) -> Option<TerminalSize> {
        ViewportRenderer::reported_size(stdout_fd)
    }

    fn bar(&self) -> Bar {
        build_host_bar(&self.addr, &self.command, self.client_count)
    }
}

fn build_host_bar(addr: &str, command: &str, client_count: usize) -> Bar {
    let clients_segment = match client_count {
        0 => "0 clients".to_string(),
        1 => "1 client".to_string(),
        n => format!("{n} clients"),
    };
    let mut bar = Bar::new(Chip::Host).segment(addr);
    if !command.is_empty() {
        bar = bar.segment(command);
    }
    bar.segment(clients_segment)
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
        Some(TerminalSize::new(size.ws_col, size.ws_row))
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

#[derive(Debug, Default)]
struct JoinReplay {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl JoinReplay {
    fn remember(&mut self, frame: &[u8]) {
        self.frames.push_back(frame.to_vec());
        self.bytes = self.bytes.saturating_add(frame.len());
        while self.bytes > MAX_JOIN_REPLAY_BYTES {
            let Some(dropped) = self.frames.pop_front() else {
                self.bytes = 0;
                break;
            };
            self.bytes = self.bytes.saturating_sub(dropped.len());
        }
    }

    fn bytes(&self) -> usize {
        self.bytes
    }

    fn frames(&self) -> impl Iterator<Item = &[u8]> {
        self.frames.iter().map(Vec::as_slice)
    }
}

#[derive(Debug)]
struct Client {
    stream: TcpStream,
    pending: VecDeque<u8>,
    input_pending: Vec<u8>,
    input_open: bool,
    protocol_ready: bool,
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
            protocol_ready: false,
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

    fn enqueue_replay(&mut self, replay: &JoinReplay) -> bool {
        if replay.bytes() > MAX_CLIENT_BACKLOG_BYTES.saturating_sub(self.pending.len()) {
            return false;
        }
        for frame in replay.frames() {
            self.pending.extend(frame.iter().copied());
        }
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
                    if !self.flush_pending_input(pty_fd)? {
                        return Ok(false);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }
    }

    fn flush_pending_input(&mut self, pty_fd: i32) -> anyhow::Result<bool> {
        loop {
            if self.input_pending.is_empty() {
                return Ok(true);
            }
            let Some(start) =
                room_protocol::find_subslice(&self.input_pending, room_protocol::PREFIX)
            else {
                if !self.protocol_ready {
                    let keep =
                        room_protocol::prefix_overlap(&self.input_pending, room_protocol::PREFIX);
                    return Ok(
                        keep > 0 && self.input_pending.len() <= room_protocol::MAX_CONTROL_BYTES
                    );
                }
                let keep =
                    room_protocol::prefix_overlap(&self.input_pending, room_protocol::PREFIX);
                let write_len = self.input_pending.len().saturating_sub(keep);
                if write_len > 0 {
                    write_all(pty_fd, &self.input_pending[..write_len])?;
                    self.input_pending.drain(..write_len);
                }
                return Ok(true);
            };
            if start > 0 {
                if !self.protocol_ready {
                    return Ok(false);
                }
                write_all(pty_fd, &self.input_pending[..start])?;
                self.input_pending.drain(..start);
                continue;
            }

            let suffix_search_start = room_protocol::PREFIX.len();
            let Some(end_rel) = room_protocol::find_subslice(
                &self.input_pending[suffix_search_start..],
                room_protocol::SUFFIX,
            ) else {
                if self.input_pending.len() > room_protocol::MAX_CONTROL_BYTES {
                    if !self.protocol_ready {
                        return Ok(false);
                    }
                    write_all(pty_fd, &self.input_pending[..1])?;
                    self.input_pending.drain(..1);
                    continue;
                }
                return Ok(true);
            };
            let payload_start = room_protocol::PREFIX.len();
            let payload_end = suffix_search_start + end_rel;
            let payload = self.input_pending[payload_start..payload_end].to_vec();
            if !self.apply_control(&payload) {
                return Ok(false);
            }
            self.input_pending
                .drain(..payload_end + room_protocol::SUFFIX.len());
        }
    }

    fn flush_pending_input_as_raw(&mut self, pty_fd: i32) -> anyhow::Result<()> {
        if !self.protocol_ready {
            self.input_pending.clear();
            return Ok(());
        }
        if !self.input_pending.is_empty() {
            write_all(pty_fd, &self.input_pending)?;
            self.input_pending.clear();
        }
        Ok(())
    }

    fn apply_control(&mut self, payload: &[u8]) -> bool {
        match room_protocol::parse_client_control(payload) {
            Some(ClientControl::Hello(version)) => {
                if version == room_protocol::VERSION {
                    self.protocol_ready = true;
                    return true;
                }
                false
            }
            Some(ClientControl::Resize(size)) => {
                if self.protocol_ready {
                    self.size = Some(size);
                    true
                } else {
                    false
                }
            }
            None => self.protocol_ready,
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
    host_viewport: Option<&'a mut HostViewport>,
    join_replay: &'a mut JoinReplay,
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
    use std::os::fd::{AsFd, AsRawFd};

    use super::*;
    use crate::trace::{EventKind, Trace};
    use nix::unistd::{pipe, read as nix_read};

    #[test]
    fn default_share_opts_bind_a_trace_name() {
        assert_eq!(ShareOpts::default().out, PathBuf::from("shared.ptytrace"));
        assert!(ShareOpts::default().local_input);
    }

    #[test]
    fn share_rejects_zero_initial_size() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let err = run(
            &listener,
            ShareOpts {
                cols: 0,
                rows: 24,
                local_output: false,
                local_input: false,
                ..ShareOpts::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("nonzero"));
    }

    #[test]
    fn host_bar_includes_chip_addr_command_and_client_count() {
        let bar = build_host_bar("127.0.0.1:7373", "bash -i", 0);
        let rendered =
            crate::pty::status_bar::render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered).to_string();

        assert!(text.contains(" HOST "));
        assert!(text.contains("\x1b[1;32m"));
        assert!(text.contains("127.0.0.1:7373"));
        assert!(text.contains("bash -i"));
        assert!(text.contains("0 clients"));
    }

    #[test]
    fn host_bar_uses_singular_for_one_client() {
        let bar = build_host_bar("127.0.0.1:7373", "bash", 1);
        let rendered =
            crate::pty::status_bar::render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("1 client"));
        assert!(!text.contains("1 clients"));
    }

    #[test]
    fn host_bar_omits_command_segment_when_empty() {
        let bar = build_host_bar("127.0.0.1:7373", "", 2);
        let rendered =
            crate::pty::status_bar::render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("127.0.0.1:7373"));
        assert!(text.contains("2 clients"));
    }

    #[test]
    fn host_local_io_notice_warns_for_viewer_only_host_modes() {
        assert_eq!(host_local_io_notice(true, true), None);
        assert_eq!(host_local_io_notice(true, false), None);
        assert_eq!(
            host_local_io_notice(false, true),
            Some(
                "[warning: host input disabled; type from a connected client or remove --no-local-input]"
            )
        );
        assert_eq!(
            host_local_io_notice(false, false),
            Some("[host input/output disabled; session is controlled by connected clients]")
        );
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
    fn client_raw_input_before_protocol_hello_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        let (pty_r, pty_w) = pipe().unwrap();

        peer.write_all(b"legacy input\n").unwrap();

        assert!(!client.drain_input_to_pty(pty_w.as_raw_fd()).unwrap());
        drop(pty_w);
        let mut buf = [0_u8; 16];
        assert_eq!(nix_read(&pty_r, &mut buf).unwrap(), 0);
    }

    #[test]
    fn client_accepts_protocol_hello_before_raw_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        let (pty_r, pty_w) = pipe().unwrap();
        let mut input = room_protocol::encode_hello_control();
        input.extend_from_slice(b"hello\n");

        peer.write_all(&input).unwrap();
        assert!(client.drain_input_to_pty(pty_w.as_raw_fd()).unwrap());
        drop(pty_w);

        let mut output = [0_u8; 16];
        let n = nix_read(&pty_r, &mut output).unwrap();
        assert_eq!(&output[..n], b"hello\n");
    }

    #[test]
    fn client_accepts_split_protocol_hello_before_raw_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        let (pty_r, pty_w) = pipe().unwrap();

        peer.write_all(b"\x1bPpty").unwrap();
        assert!(client.drain_input_to_pty(pty_w.as_raw_fd()).unwrap());
        peer.write_all(b"room;hello;1\x1b\\hello\n").unwrap();
        assert!(client.drain_input_to_pty(pty_w.as_raw_fd()).unwrap());
        drop(pty_w);

        assert_eq!(read_pipe_to_end(&pty_r), b"hello\n");
    }

    #[test]
    fn client_resize_before_protocol_hello_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        let (pty_r, pty_w) = pipe().unwrap();

        peer.write_all(&room_protocol::encode_resize_control(TerminalSize {
            cols: 40,
            rows: 10,
        }))
        .unwrap();

        assert!(!client.drain_input_to_pty(pty_w.as_raw_fd()).unwrap());
        drop(pty_w);
        assert!(read_pipe_to_end(&pty_r).is_empty());
    }

    #[test]
    fn client_unsupported_protocol_hello_disconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        let (pty_r, pty_w) = pipe().unwrap();

        peer.write_all(b"\x1bPptyroom;hello;2\x1b\\hello\n")
            .unwrap();

        assert!(!client.drain_input_to_pty(pty_w.as_raw_fd()).unwrap());
        drop(pty_w);
        assert!(read_pipe_to_end(&pty_r).is_empty());
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
        let payload = b"before\x1bPptyroom;size;1;1\x1b\\after";
        let mut expected = Vec::new();
        expected.extend_from_slice(room_protocol::PREFIX);
        expected.extend_from_slice(format!("data;{}", payload.len()).as_bytes());
        expected.extend_from_slice(room_protocol::SUFFIX);
        expected.extend_from_slice(payload);

        assert_eq!(room_protocol::encode_output_frame(payload), expected);
    }

    #[test]
    fn join_replay_evicts_whole_frames() {
        let mut replay = JoinReplay::default();
        let first_payload = vec![b'a'; MAX_JOIN_REPLAY_BYTES - 128];
        let first = room_protocol::encode_output_frame(&first_payload);
        let second = room_protocol::encode_output_frame(&vec![b'b'; 256]);

        replay.remember(&first);
        replay.remember(&second);

        let frames = replay
            .frames()
            .map(<[u8]>::to_vec)
            .collect::<Vec<Vec<u8>>>();
        assert_eq!(frames, vec![second]);
        assert!(replay.bytes() <= MAX_JOIN_REPLAY_BYTES);
    }

    #[test]
    fn client_replay_enqueue_preserves_frame_boundaries() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut client = Client::new(stream).unwrap();
        let mut replay = JoinReplay::default();
        let first = room_protocol::encode_output_frame(b"one");
        let second = room_protocol::encode_output_frame(b"two");
        replay.remember(&first);
        replay.remember(&second);

        assert!(client.enqueue_replay(&replay));

        let queued = client.pending.iter().copied().collect::<Vec<_>>();
        assert_eq!(queued, [first, second].concat());
    }

    fn connect_with_retry(addr: SocketAddr) -> TcpStream {
        let started = Instant::now();
        loop {
            match TcpStream::connect(addr) {
                Ok(mut stream) => {
                    stream
                        .write_all(&room_protocol::encode_hello_control())
                        .unwrap();
                    return stream;
                }
                Err(err) if started.elapsed() < Duration::from_secs(2) => {
                    assert!(
                        err.kind() == std::io::ErrorKind::ConnectionRefused
                            || err.kind() == std::io::ErrorKind::TimedOut
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("connect to ptyroom test server failed: {err}"),
            }
        }
    }

    fn read_pipe_to_end(fd: &impl AsFd) -> Vec<u8> {
        let mut output = Vec::new();
        let mut buf = [0_u8; 64];
        loop {
            match nix_read(fd, &mut buf).unwrap() {
                0 => return output,
                n => output.extend_from_slice(&buf[..n]),
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
                Err(err) => panic!("read from ptyroom client stream failed: {err}"),
            }
        }
    }

    fn resize_control(cols: u16, rows: u16) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(room_protocol::PREFIX);
        frame.extend_from_slice(format!("resize;{cols};{rows}").as_bytes());
        frame.extend_from_slice(room_protocol::SUFFIX);
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
