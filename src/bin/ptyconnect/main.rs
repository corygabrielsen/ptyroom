//! `ptyconnect` CLI: attach to a `ptyshare` session.

use std::io;
use std::io::IsTerminal;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

use anyhow::anyhow;
use clap::Parser;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{read, write};

#[derive(Parser)]
#[command(
    version,
    about = "ptyconnect — connect your terminal to a ptyshare session",
    long_about = "Connect stdin/stdout to a `ptyshare` TCP session. The transport\n\
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
    let stdin_fd = stdin.as_raw_fd();
    let stdout_fd = io::stdout().as_raw_fd();
    let _raw = if stdin.is_terminal() {
        RawModeGuard::enter(stdin_fd).ok()
    } else {
        None
    };
    relay_fds(stream, stdin_fd, stdout_fd)
}

fn relay_fds(stream: &TcpStream, stdin_fd: RawFd, stdout_fd: RawFd) -> anyhow::Result<()> {
    let stream_fd = stream.as_raw_fd();
    let mut buf = [0_u8; 4096];
    let mut stdin_open = true;

    loop {
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
        match poll(&mut fds, PollTimeout::NONE) {
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
            if rev.intersects(PollFlags::POLLIN) {
                let stream_borrow = unsafe { BorrowedFd::borrow_raw(stream_fd) };
                match read(stream_borrow, &mut buf) {
                    Ok(0) | Err(Errno::EIO) => return Ok(()),
                    Ok(n) => write_all(stdout_fd, &buf[..n])?,
                    Err(Errno::EINTR) => {}
                    Err(err) => return Err(anyhow!("read ptyshare socket: {err}")),
                }
            }
            if rev.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
                return Ok(());
            }
        }
    }
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

    use super::{Args, relay_fds};

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
}
