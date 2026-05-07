//! `ptyconnect` CLI: attach to a `ptyshare` session.

use std::io;
use std::net::{SocketAddr, TcpStream};
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
    let stdin_fd = io::stdin().as_raw_fd();
    let stdout_fd = io::stdout().as_raw_fd();
    let stream_fd = stream.as_raw_fd();
    let _raw = RawModeGuard::enter(stdin_fd).ok();
    let mut buf = [0_u8; 4096];

    loop {
        let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
        let stream_borrow = unsafe { BorrowedFd::borrow_raw(stream_fd) };
        let mut fds = [
            PollFd::new(stdin_borrow, PollFlags::POLLIN),
            PollFd::new(stream_borrow, PollFlags::POLLIN),
        ];
        match poll(&mut fds, PollTimeout::NONE) {
            Ok(_) | Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow!("poll ptyconnect: {err}")),
        }

        if fds[0]
            .revents()
            .is_some_and(|rev| rev.intersects(PollFlags::POLLIN))
        {
            let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
            match read(stdin_borrow, &mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => write_all(stream_fd, &buf[..n])?,
                Err(Errno::EINTR) => {}
                Err(err) => return Err(anyhow!("read stdin: {err}")),
            }
        }

        if let Some(rev) = fds[1].revents() {
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
    use clap::{CommandFactory, Parser};

    use super::Args;

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
}
