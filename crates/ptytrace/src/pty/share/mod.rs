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

mod client;
mod ctl;
mod host_viewport;
mod pending;
mod poll_loop;
mod pty_output;
mod sizing;

use std::collections::VecDeque;
use std::io::{self, BufReader, IsTerminal, Write};
#[cfg(test)]
use std::net::TcpStream;
use std::net::{Shutdown, SocketAddr, TcpListener};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use nix::errno::Errno;
use nix::poll::PollFlags;
use nix::unistd::{geteuid, read};

#[cfg(test)]
use self::client::broadcast;
use self::client::{Client, JoinReplay, ShareStats};
use self::ctl::{CtlCommand, CtlSocket, QueueOp, parse_ctl_command};
use self::host_viewport::HostViewport;
use self::pending::PendingState;
use self::poll_loop::{accept_ready_clients, poll_share_fds, process_client_revents};
use self::pty_output::{PtyOutputSinks, handle_pty_revents};
use self::sizing::{initial_host_size, initial_pty_size, refresh_host_size, sync_canonical_size};
use super::input_router::{LocalInputAction, LocalInputRouter, LocalStatus};
use super::process;
#[cfg(test)]
use super::room_protocol;
use super::room_protocol::TerminalSize;
use super::terminal_io::write_all;
use super::terminal_state::{
    RawModeGuard, RestoreGuard, child_output_cleanup_guard, termination_requested,
};
use crate::recording::TraceBuilder;

const CTL_IO_TIMEOUT: Duration = Duration::from_millis(500);

/// RAII wrapper that calls `shutdown(Shutdown::Both)` on the wrapped
/// `UnixStream` when the guard goes out of scope, regardless of
/// whether scope exit was a normal return, an early return on error,
/// or stack unwinding from a panic. Used in `handle_ctl_connection`
/// so the peer never sees a half-open ctl socket.
struct CtlStreamGuard {
    stream: UnixStream,
}

impl CtlStreamGuard {
    fn new(stream: UnixStream) -> Self {
        Self { stream }
    }
}

impl Drop for CtlStreamGuard {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// Resolve the directory for ptyroom runtime state (control sockets,
/// etc.).
///
/// Precedence, highest to lowest:
///   1. `override_dir` (e.g. from a `--state-dir` CLI flag)
///   2. `PTYROOM_STATE_DIR` environment variable
///   3. `$XDG_RUNTIME_DIR/ptyroom/` when `XDG_RUNTIME_DIR` is set
///   4. `/tmp/ptyroom-<euid>/` (tmux-style per-user fallback)
///
/// This function is pure resolution; callers create the directory
/// themselves (see `CtlSocket::bind`).
#[must_use]
pub fn resolve_state_dir(override_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = override_dir {
        return dir.to_path_buf();
    }
    if let Some(env) = std::env::var_os("PTYROOM_STATE_DIR")
        && !env.is_empty()
    {
        return PathBuf::from(env);
    }
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("ptyroom");
    }
    PathBuf::from(format!("/tmp/ptyroom-{}", geteuid().as_raw()))
}

/// Filesystem path of the local control socket for a ptyroom host bound
/// to `port`, under `state_dir`.
///
/// Shared between the host (which creates the socket) and the `ptyroom
/// ctl` subcommand (which connects to it). Localhost only by design.
#[must_use]
pub fn ctl_socket_path(state_dir: &Path, port: u16) -> PathBuf {
    state_dir.join(format!("{port}.sock"))
}

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
    /// Override directory for runtime state (control socket, etc.).
    /// `None` uses the precedence in [`resolve_state_dir`].
    pub state_dir: Option<PathBuf>,
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
            state_dir: None,
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
    input_router: Option<LocalInputRouter>,
    ctl_socket: Option<CtlSocket>,
    queue: VecDeque<String>,
    local_stdin_open: bool,
    should_end: bool,
    clients: Vec<Client>,
    join_replay: JoinReplay,
    stats: ShareStats,
    builder: TraceBuilder,
    pending: PendingState,
    listen_addr: SocketAddr,
    out_path: PathBuf,
    started: Instant,
    max_runtime: Duration,
    _terminal_cleanup: Option<RestoreGuard>,
    _raw_mode: Option<RawModeGuard>,
}

impl<'a> Session<'a> {
    fn start(listener: &'a TcpListener, opts: ShareOpts) -> anyhow::Result<Self> {
        process::ensure_nonzero_size(opts.cols, opts.rows)?;
        listener.set_nonblocking(true)?;
        let listen_addr = listener.local_addr()?;
        let argv = process::resolve_argv(opts.argv);
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let listener_fd = listener.as_raw_fd();
        let stdin = io::stdin();
        let stdin_fd = stdin.as_raw_fd();
        let stdout = std::io::stdout();
        let stdout_fd = stdout.as_raw_fd();
        let (host_viewport, terminal_cleanup) =
            setup_host_terminal(opts.local_output, &argv, listen_addr, &stdout, stdout_fd)?;
        let raw_mode = host_raw_mode_guard(opts.local_input, &stdin, stdin_fd)?;
        let initial_size =
            initial_pty_size(opts.cols, opts.rows, host_viewport.as_ref(), stdout_fd);
        let pty = process::spawn(&argv_refs, initial_size.cols, initial_size.rows)?;
        let pty_fd = pty.fd();
        let host_size = initial_host_size(
            opts.local_output,
            &stdout,
            stdout_fd,
            host_viewport.as_ref(),
        );
        let started = Instant::now();
        let input_router =
            (host_viewport.is_some() && raw_mode.is_some()).then(LocalInputRouter::default);
        let mut host_viewport = host_viewport;
        if let Some(view) = host_viewport.as_mut() {
            view.set_controls_enabled(input_router.is_some());
        }
        let ctl_socket = if cfg!(test) {
            None
        } else {
            let state_dir = resolve_state_dir(opts.state_dir.as_deref());
            match CtlSocket::bind(&state_dir, listen_addr.port()) {
                Ok(socket) => Some(socket),
                Err(err) => {
                    eprintln!("[ptyroom: control socket disabled: {err}]");
                    None
                }
            }
        };
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
            input_router,
            ctl_socket,
            queue: VecDeque::new(),
            local_stdin_open: opts.local_input,
            should_end: false,
            clients: Vec::new(),
            join_replay: JoinReplay::default(),
            stats: ShareStats::default(),
            builder: TraceBuilder::new(),
            // PendingState starts empty; the first PTY-read/resize is
            // buffered, and its dwell is measured against the *next*
            // event's arrival rather than against `started`. Fixes the
            // bug where the first event's dwell absorbed session
            // bootstrap latency (same bug pattern as live mode, commit
            // `26b840b`).
            pending: PendingState::default(),
            listen_addr,
            out_path: opts.out,
            started,
            max_runtime: opts.max_runtime,
            _terminal_cleanup: terminal_cleanup,
            _raw_mode: raw_mode,
        })
    }

    fn should_stop(&self) -> bool {
        self.should_end || termination_requested() || self.started.elapsed() > self.max_runtime
    }

    fn drain_host_input(&mut self, buf: &mut [u8]) -> anyhow::Result<bool> {
        let stdin_borrow = unsafe { BorrowedFd::borrow_raw(self.stdin_fd) };
        match read(stdin_borrow, buf) {
            Ok(0) | Err(Errno::EIO) => Ok(false),
            Ok(n) => {
                self.process_host_input(&buf[..n])?;
                Ok(true)
            }
            Err(Errno::EINTR) => Ok(true),
            Err(err) => Err(anyhow!("read local stdin: {err}")),
        }
    }

    fn tick_local_input_router(&mut self) -> anyhow::Result<()> {
        let Some(router) = self.input_router.as_mut() else {
            return Ok(());
        };
        if let Some(LocalInputAction::SetStatus(status)) = router.tick(std::time::Instant::now())
            && let Some(view) = self.host_viewport.as_mut()
        {
            view.set_status(self.stdout_fd, status)?;
        }
        Ok(())
    }

    fn process_host_input(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        let Some(router) = self.input_router.as_mut() else {
            return write_all(self.pty_fd, bytes);
        };
        let actions: Vec<LocalInputAction> = bytes.iter().map(|&b| router.push(b)).collect();
        let mut remote = Vec::with_capacity(actions.len());
        for action in actions {
            if let LocalInputAction::Remote(b) = action {
                remote.push(b);
                continue;
            }
            if !remote.is_empty() {
                write_all(self.pty_fd, &remote)?;
                remote.clear();
            }
            match action {
                LocalInputAction::SetStatus(status) => {
                    if let Some(view) = self.host_viewport.as_mut() {
                        view.set_status(self.stdout_fd, status)?;
                    }
                }
                LocalInputAction::ForceRedraw => {
                    if let Some(view) = self.host_viewport.as_mut() {
                        view.set_status(self.stdout_fd, LocalStatus::Connected)?;
                        view.force_redraw(self.stdout_fd)?;
                    }
                }
                LocalInputAction::Disconnect => {
                    self.should_end = true;
                    return Ok(());
                }
                LocalInputAction::UnknownCommand(_) => {
                    if let Some(view) = self.host_viewport.as_mut() {
                        view.set_status(self.stdout_fd, LocalStatus::Connected)?;
                    }
                }
                LocalInputAction::Remote(_) => unreachable!(),
            }
        }
        if !remote.is_empty() {
            write_all(self.pty_fd, &remote)?;
        }
        Ok(())
    }

    fn maybe_drain_host_input(
        &mut self,
        revents: PollFlags,
        buf: &mut [u8],
    ) -> anyhow::Result<bool> {
        if self.local_stdin_open && revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
            self.drain_host_input(buf)
        } else {
            Ok(self.local_stdin_open)
        }
    }

    fn drain_ctl_socket(&mut self) -> anyhow::Result<()> {
        loop {
            let next = match self.ctl_socket.as_ref() {
                None => return Ok(()),
                Some(socket) => socket.listener.accept(),
            };
            match next {
                Ok((stream, _addr)) => self.handle_ctl_connection(stream),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => return Err(anyhow!("accept ptyroom control connection: {err}")),
            }
        }
    }

    fn handle_ctl_connection(&mut self, stream: UnixStream) {
        // `CtlStreamGuard` calls `shutdown(Both)` on drop so even a
        // panic inside `parse_ctl_command` or `execute_ctl_command`
        // closes the socket cleanly. Pre-fix, only the happy match
        // arm shut the stream down; any error path that unwound past
        // the explicit call leaked a half-open peer connection.
        let mut guard = CtlStreamGuard::new(stream);
        guard.stream.set_read_timeout(Some(CTL_IO_TIMEOUT)).ok();
        guard.stream.set_write_timeout(Some(CTL_IO_TIMEOUT)).ok();
        let parse_result = {
            let mut reader = BufReader::new(&mut guard.stream);
            parse_ctl_command(&mut reader)
        };
        let response = match parse_result {
            Ok(cmd) => match self.execute_ctl_command(cmd) {
                Ok(line) => format!("ok {line}\n"),
                Err(err) => format!("err {err}\n"),
            },
            Err(err) => format!("err {err}\n"),
        };
        let _ = guard.stream.write_all(response.as_bytes());
        // Explicit drop documents that shutdown runs here on the happy
        // path too; the Drop impl makes the error paths equivalent.
        drop(guard);
    }

    fn execute_ctl_command(&mut self, cmd: CtlCommand) -> anyhow::Result<String> {
        match cmd {
            CtlCommand::Queue(op) => self.execute_queue_op(op),
        }
    }

    fn execute_queue_op(&mut self, op: QueueOp) -> anyhow::Result<String> {
        match op {
            QueueOp::Add(text) => {
                self.queue.push_back(text);
                self.refresh_queue_status()?;
                Ok(format!("queue-depth={}", self.queue.len()))
            }
            QueueOp::Next => {
                if let Some(text) = self.queue.pop_front() {
                    write_all(self.pty_fd, text.as_bytes())?;
                    write_all(self.pty_fd, b"\r")?;
                    self.refresh_queue_status()?;
                    Ok(format!("injected-bytes={}", text.len() + 1))
                } else {
                    Ok("queue-empty".to_string())
                }
            }
            QueueOp::List => Ok(format!("queue-depth={}", self.queue.len())),
            QueueOp::Clear => {
                let n = self.queue.len();
                self.queue.clear();
                self.refresh_queue_status()?;
                Ok(format!("cleared={n}"))
            }
        }
    }

    fn refresh_queue_status(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.host_viewport.as_mut() {
            view.set_queue_depth(self.queue.len())?;
        }
        Ok(())
    }

    fn tick(&mut self, buf: &mut [u8]) -> anyhow::Result<bool> {
        // Drive the local-input router's idle timeout so a lone
        // `Ctrl-]` does not silently arm Command mode until the host
        // types again. See `COMMAND_MODE_TIMEOUT` in `input_router.rs`.
        self.tick_local_input_router()?;
        refresh_host_size(
            self.local_output,
            self.host_viewport.is_some(),
            self.stdout_fd,
            &mut self.host_size,
            &mut self.last_size_check,
        );
        self.drain_ctl_socket()?;
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
            &mut self.pending,
            &mut self.stats,
        )?;
        self.local_stdin_open = self.maybe_drain_host_input(poll_state.stdin_revents, buf)?;
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
            &mut self.pending,
        )
    }

    fn finish(mut self) -> anyhow::Result<ShareSummary> {
        // No more events will arrive — flush the buffered event with
        // dwell 0 before sealing the trace. Asciinema players hold
        // the final frame indefinitely.
        self.pending.flush_final(&mut self.builder)?;
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
        let cleanup = child_output_cleanup_guard(local_output, stdout_fd);
        Ok((None, cleanup))
    }
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

fn finish_share_trace(
    builder: TraceBuilder,
    size: TerminalSize,
    out: PathBuf,
) -> anyhow::Result<(PathBuf, usize)> {
    let recording = builder.finish_screen(size.cols, size.rows)?;
    let trace = recording.into_trace();
    let events = trace.events.len();
    trace.write(&out)?;
    Ok((out, events))
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read};
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
        let bar = host_viewport::build_host_bar(
            "127.0.0.1:7373",
            "bash -i",
            0,
            0,
            LocalStatus::Connected,
            false,
        );
        let rendered =
            crate::pty::status_bar::render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered).to_string();

        assert!(text.contains(" HOST "));
        assert!(text.contains("\x1b[1;32m"));
        assert!(text.contains("127.0.0.1:7373"));
        assert!(text.contains("bash -i"));
        assert!(text.contains("0 clients"));
        assert!(!text.contains("queued"));
    }

    #[test]
    fn host_bar_uses_singular_for_one_client() {
        let bar = host_viewport::build_host_bar(
            "127.0.0.1:7373",
            "bash",
            1,
            0,
            LocalStatus::Connected,
            false,
        );
        let rendered =
            crate::pty::status_bar::render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("1 client"));
        assert!(!text.contains("1 clients"));
    }

    #[test]
    fn host_bar_shows_help_hint_when_controls_enabled() {
        let bar = host_viewport::build_host_bar(
            "127.0.0.1:7373",
            "bash",
            0,
            0,
            LocalStatus::Connected,
            true,
        );
        let rendered =
            crate::pty::status_bar::render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("^] ? help"));
    }

    #[test]
    fn host_bar_command_state_lists_end_redraw_send() {
        let bar = host_viewport::build_host_bar(
            "127.0.0.1:7373",
            "bash",
            0,
            0,
            LocalStatus::Command,
            true,
        );
        let rendered = crate::pty::status_bar::render(
            &bar,
            Some(TerminalSize {
                cols: 120,
                rows: 24,
            }),
        );
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains(". end"));
        assert!(text.contains("r redraw"));
        assert!(text.contains("^] send"));
    }

    #[test]
    fn parse_ctl_next_returns_queue_next() {
        let mut reader = std::io::Cursor::new(b"next\n".to_vec());
        assert_eq!(
            parse_ctl_command(&mut reader).unwrap(),
            CtlCommand::Queue(QueueOp::Next)
        );
    }

    #[test]
    fn parse_ctl_add_reads_length_prefixed_payload() {
        let mut reader = std::io::Cursor::new(b"add 5\nhello".to_vec());
        assert_eq!(
            parse_ctl_command(&mut reader).unwrap(),
            CtlCommand::Queue(QueueOp::Add("hello".to_string()))
        );
    }

    #[test]
    fn parse_ctl_add_accepts_multiline_payload() {
        let mut reader = std::io::Cursor::new(b"add 11\nline1\nline2".to_vec());
        assert_eq!(
            parse_ctl_command(&mut reader).unwrap(),
            CtlCommand::Queue(QueueOp::Add("line1\nline2".to_string()))
        );
    }

    #[test]
    fn parse_ctl_add_rejects_oversize_payload() {
        let header = format!("add {}\n", ctl::CTL_MAX_PAYLOAD_BYTES + 1);
        let mut reader = std::io::Cursor::new(header.into_bytes());
        let err = parse_ctl_command(&mut reader).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn parse_ctl_unknown_verb_errors() {
        let mut reader = std::io::Cursor::new(b"frobnicate\n".to_vec());
        let err = parse_ctl_command(&mut reader).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn parse_ctl_strips_leading_whitespace() {
        // Leading spaces/tabs in front of the verb should not produce
        // an empty-verb error.
        let mut reader = std::io::Cursor::new(b"  \tnext\n".to_vec());
        assert_eq!(
            parse_ctl_command(&mut reader).unwrap(),
            CtlCommand::Queue(QueueOp::Next)
        );
    }

    #[test]
    fn parse_ctl_rejects_unbounded_line() {
        // No newline ever — `read_line` would otherwise grow without
        // limit. The cap should kick in and surface as an error.
        let huge = vec![b'x'; ctl::MAX_CTL_LINE_BYTES + 1];
        let mut reader = std::io::Cursor::new(huge);
        let err = parse_ctl_command(&mut reader).unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn parse_ctl_clear_and_list_round_trip() {
        let mut clear = std::io::Cursor::new(b"clear\n".to_vec());
        let mut list = std::io::Cursor::new(b"list\n".to_vec());
        assert_eq!(
            parse_ctl_command(&mut clear).unwrap(),
            CtlCommand::Queue(QueueOp::Clear)
        );
        assert_eq!(
            parse_ctl_command(&mut list).unwrap(),
            CtlCommand::Queue(QueueOp::List)
        );
    }

    #[test]
    fn host_bar_shows_queue_depth_when_nonempty() {
        let bar = host_viewport::build_host_bar(
            "127.0.0.1:7373",
            "bash",
            0,
            3,
            LocalStatus::Connected,
            false,
        );
        let rendered =
            crate::pty::status_bar::render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("3 queued"));
    }

    #[test]
    fn host_bar_omits_command_segment_when_empty() {
        let bar = host_viewport::build_host_bar(
            "127.0.0.1:7373",
            "",
            2,
            0,
            LocalStatus::Connected,
            false,
        );
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
                        state_dir: None,
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
        assert!(client.enqueue(&vec![b'x'; client::MAX_CLIENT_BACKLOG_BYTES]));
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
            sizing::desired_session_size(fallback, None, &[small, large]),
            TerminalSize { cols: 40, rows: 10 }
        );

        let large = client_with_size(TerminalSize { cols: 90, rows: 25 });
        assert_eq!(
            sizing::desired_session_size(fallback, None, &[large]),
            TerminalSize { cols: 90, rows: 25 }
        );
        assert_eq!(sizing::desired_session_size(fallback, None, &[]), fallback);
    }

    #[test]
    fn desired_session_size_ignores_per_axis_zeros() {
        // A zero on one axis means "I don't know this dimension yet."
        // Two clients each missing one axis should compose to a
        // sensible non-zero size, not collapse the PTY to (0, 24).
        let fallback = TerminalSize {
            cols: 100,
            rows: 30,
        };
        let cols_only = client_with_size(TerminalSize { cols: 80, rows: 0 });
        let rows_only = client_with_size(TerminalSize { cols: 0, rows: 24 });

        assert_eq!(
            sizing::desired_session_size(fallback, None, &[cols_only, rows_only]),
            TerminalSize { cols: 80, rows: 24 }
        );

        // If every participant has both axes zero, fall back.
        let blank = client_with_size(TerminalSize { cols: 0, rows: 0 });
        assert_eq!(
            sizing::desired_session_size(fallback, None, &[blank]),
            fallback
        );
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
        let first_payload = vec![b'a'; client::MAX_JOIN_REPLAY_BYTES - 128];
        let first = room_protocol::encode_output_frame(&first_payload);
        let second = room_protocol::encode_output_frame(&vec![b'b'; 256]);

        replay.remember(&first);
        replay.remember(&second);

        let frames = replay
            .frames()
            .map(<[u8]>::to_vec)
            .collect::<Vec<Vec<u8>>>();
        assert_eq!(frames, vec![second]);
        assert!(replay.bytes() <= client::MAX_JOIN_REPLAY_BYTES);
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
