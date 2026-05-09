#![allow(dead_code)]

use std::io::{BufReader, Read as _};
use std::os::fd::AsFd;
use std::process::{Child, ExitStatus, Output};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::read as nix_read;

pub fn wait_with_timeout(mut child: Child, timeout: Duration) -> Output {
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            panic!("process did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn wait_status_with_timeout(child: &mut Child, timeout: Duration) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            panic!("process did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn drain_remaining_stderr<R>(mut reader: BufReader<R>)
where
    R: std::io::Read + Send + 'static,
{
    let _ = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
    });
}

pub fn wait_child_stdout_until(child: &mut Child, needle: &[u8], timeout: Duration) -> Vec<u8> {
    let stdout = child.stdout.as_ref().expect("child stdout must be piped");
    read_fd_until(stdout, needle, timeout)
}

pub fn read_fd_until(fd: &impl AsFd, needle: &[u8], timeout: Duration) -> Vec<u8> {
    let started = Instant::now();
    let mut output = Vec::new();
    let mut buf = [0_u8; 256];
    while find_subslice(&output, needle).is_none() {
        assert!(started.elapsed() <= timeout, "output timed out");
        let mut poll_fd = [PollFd::new(fd.as_fd(), PollFlags::POLLIN)];
        poll(&mut poll_fd, PollTimeout::from(50_u16)).unwrap();
        let Some(revents) = poll_fd[0].revents() else {
            continue;
        };
        if !revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
            continue;
        }
        match nix_read(fd, &mut buf) {
            Ok(0) | Err(Errno::EIO) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(err) => panic!("read output failed: {err}"),
        }
    }
    output
}

pub fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_subslice(haystack, needle).is_some()
}

pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
