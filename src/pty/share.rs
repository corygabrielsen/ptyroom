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

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::{read, write};

use super::process;
use crate::recording::{DwellMs, TraceBuilder};

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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSummary {
    pub listen_addr: SocketAddr,
    pub trace_path: PathBuf,
    pub events: usize,
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
    let stdout_fd = std::io::stdout().as_raw_fd();
    let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
    let mut clients: Vec<TcpStream> = Vec::new();
    let mut builder = TraceBuilder::new();
    let started = Instant::now();
    let mut last_event = started;
    let mut buf = [0_u8; 4096];

    loop {
        if started.elapsed() > opts.max_runtime {
            break;
        }

        let listener_borrow = unsafe { BorrowedFd::borrow_raw(listener_fd) };
        let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        let mut fds = [
            PollFd::new(listener_borrow, PollFlags::POLLIN),
            PollFd::new(pty_borrow, PollFlags::POLLIN),
        ];
        match poll(&mut fds, PollTimeout::from(50_u16)) {
            Ok(_) | Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("poll shared PTY: {err}")),
        }

        if fds[0]
            .revents()
            .is_some_and(|rev| rev.intersects(PollFlags::POLLIN))
        {
            accept_ready_clients(listener, &input_tx, &mut clients)?;
        }

        drain_client_input(pty_fd, &input_rx)?;

        if let Some(rev) = fds[1].revents() {
            if rev.intersects(PollFlags::POLLIN) {
                let pty_borrow = unsafe { BorrowedFd::borrow_raw(pty_fd) };
                match read(pty_borrow, &mut buf) {
                    Ok(0) | Err(Errno::EIO) => break,
                    Ok(n) => {
                        let bytes = &buf[..n];
                        if opts.local_output {
                            let stdout_borrow = unsafe { BorrowedFd::borrow_raw(stdout_fd) };
                            let _ = write(stdout_borrow, bytes);
                        }
                        broadcast(&mut clients, bytes);
                        let now = Instant::now();
                        let dwell =
                            DwellMs::from_duration(now.saturating_duration_since(last_event));
                        builder.record_output(bytes.to_vec(), dwell)?;
                        last_event = now;
                    }
                    Err(Errno::EINTR) => {}
                    Err(err) => return Err(anyhow!("read shared PTY: {err}")),
                }
            }
            if rev.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
                break;
            }
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
    })
}

fn accept_ready_clients(
    listener: &TcpListener,
    input_tx: &mpsc::Sender<Vec<u8>>,
    clients: &mut Vec<TcpStream>,
) -> anyhow::Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                stream.set_nodelay(true)?;
                let reader = stream.try_clone()?;
                clients.push(stream);
                spawn_client_reader(reader, input_tx.clone());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(err) => return Err(err).context("accept ptyshare client"),
        }
    }
}

fn spawn_client_reader(mut stream: TcpStream, input_tx: mpsc::Sender<Vec<u8>>) {
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if input_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
}

fn drain_client_input(pty_fd: i32, input_rx: &mpsc::Receiver<Vec<u8>>) -> anyhow::Result<()> {
    loop {
        match input_rx.try_recv() {
            Ok(bytes) => write_all(pty_fd, &bytes)?,
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

fn broadcast(clients: &mut Vec<TcpStream>, bytes: &[u8]) {
    clients.retain_mut(|client| client.write_all(bytes).is_ok());
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
    use super::*;
    use crate::trace::Trace;

    #[test]
    fn default_share_opts_bind_a_trace_name() {
        assert_eq!(ShareOpts::default().out, PathBuf::from("shared.ptytrace"));
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
        let trace = Trace::read(summary.trace_path).unwrap();
        assert!(
            trace
                .events
                .iter()
                .any(|event| event.data.contains("got:hello"))
        );
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
}
