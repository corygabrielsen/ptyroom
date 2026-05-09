mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::AsFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::openpty;
use nix::unistd::read as nix_read;

use common::{drain_remaining_stderr, find_subslice, wait_status_with_timeout, wait_with_timeout};

const ALT_SCREEN_EXIT: &[u8] = b"\x1b[?1049l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

#[test]
fn ptyroom_host_forwards_local_stdin_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("ptyroom-host-input.ptytrace");
    let (mut host, _addr) = spawn_ptyroom_host_with_stdin(&[
        "host",
        "--listen",
        "127.0.0.1:0",
        "--out",
        trace_path.to_str().unwrap(),
        "--cols",
        "100",
        "--rows",
        "30",
        "--max-secs",
        "5",
        "sh",
        "-lc",
        "read line; printf 'host:%s\\n' \"$line\"",
    ]);
    host.stdin
        .take()
        .unwrap()
        .write_all(b"hello from host\n")
        .unwrap();

    let host_output = wait_with_timeout(host, Duration::from_secs(5));
    assert!(
        host_output.status.success(),
        "ptyroom host failed: {}",
        String::from_utf8_lossy(&host_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&host_output.stdout).contains("host:hello from host"),
        "ptyroom host stdout was {:?}",
        String::from_utf8_lossy(&host_output.stdout)
    );
    assert!(trace_path.exists());
}

#[test]
fn ptyroom_join_receives_host_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("ptyroom.ptytrace");
    let (host, addr) = spawn_ptyroom_host(&[
        "host",
        "--listen",
        "127.0.0.1:0",
        "--no-local-input",
        "--no-local-output",
        "--max-secs",
        "5",
        "--out",
        trace_path.to_str().unwrap(),
        "sh",
        "-lc",
        "printf 'ready\\n'; read line; printf 'room:%s\\n' \"$line\"",
    ]);

    let mut join = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .arg("join")
        .arg(addr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    join.stdin.take().unwrap().write_all(b"hello\n").unwrap();

    let join_output = wait_with_timeout(join, Duration::from_secs(5));
    assert!(
        join_output.status.success(),
        "ptyroom join failed: {}",
        String::from_utf8_lossy(&join_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&join_output.stdout);
    assert!(
        stdout.contains("ready") && stdout.contains("room:hello"),
        "ptyroom join stdout was {stdout:?}"
    );

    let host_output = wait_with_timeout(host, Duration::from_secs(5));
    assert!(
        host_output.status.success(),
        "ptyroom host failed: {}",
        String::from_utf8_lossy(&host_output.stderr)
    );
    assert!(trace_path.exists());
}

#[test]
fn ptyroom_host_warns_when_local_input_is_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("ptyroom-no-local-input.ptytrace");

    let output = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .args([
            "host",
            "--listen",
            "127.0.0.1:0",
            "--no-local-input",
            "--max-secs",
            "0",
            "--out",
            trace_path.to_str().unwrap(),
            "sh",
            "-lc",
            "true",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "ptyroom host failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("host input disabled"),
        "ptyroom host stderr was {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ptyroom_host_restores_terminal_on_sigint() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("ptyroom-sigint.ptytrace");
    let pty = openpty(None, None).unwrap();
    let mut host = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .args([
            "host",
            "--listen",
            "127.0.0.1:0",
            "--no-local-input",
            "--max-secs",
            "30",
            "--out",
            trace_path.to_str().unwrap(),
            "sh",
            "-lc",
            "printf ready; sleep 30",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(pty.slave))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stderr = host.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);
    let mut listening = String::new();
    reader.read_line(&mut listening).unwrap();
    assert!(
        listening.contains("[ptyroom listening on"),
        "unexpected ptyroom listening line: {listening:?}"
    );
    let mut output = read_pty_until(&pty.master, b"ready", Duration::from_secs(5));

    unsafe {
        nix::libc::kill(host.id().try_into().unwrap(), nix::libc::SIGINT);
    }
    let status = wait_status_with_timeout(&mut host, Duration::from_secs(5));
    assert!(status.success(), "ptyroom host exited with {status}");

    let mut buf = [0_u8; 256];
    loop {
        match nix_read(&pty.master, &mut buf) {
            Ok(0) | Err(Errno::EIO) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(err) => panic!("read ptyroom host pty output failed: {err}"),
        }
    }
    assert!(
        contains_ordered_bytes(&output, ALT_SCREEN_EXIT, SHOW_CURSOR),
        "terminal restore sequence missing from {:?}",
        String::from_utf8_lossy(&output)
    );
}

fn spawn_ptyroom_host(args: &[&str]) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);
    let mut listening = String::new();
    let mut join_hint = String::new();
    reader.read_line(&mut listening).unwrap();
    reader.read_line(&mut join_hint).unwrap();
    assert!(
        join_hint.contains("ptyroom join"),
        "missing ptyroom join hint: {join_hint:?}"
    );

    let addr = listening
        .trim()
        .strip_prefix("[ptyroom listening on ")
        .and_then(|line| line.strip_suffix(']'))
        .unwrap_or_else(|| panic!("unexpected ptyroom listening line: {listening:?}"))
        .to_string();
    drain_remaining_stderr(reader);

    (child, addr)
}

fn spawn_ptyroom_host_with_stdin(args: &[&str]) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);
    let mut listening = String::new();
    let mut join_hint = String::new();
    reader.read_line(&mut listening).unwrap();
    reader.read_line(&mut join_hint).unwrap();
    assert!(
        join_hint.contains("ptyroom join"),
        "missing ptyroom join hint: {join_hint:?}"
    );

    let addr = listening
        .trim()
        .strip_prefix("[ptyroom listening on ")
        .and_then(|line| line.strip_suffix(']'))
        .unwrap_or_else(|| panic!("unexpected ptyroom listening line: {listening:?}"))
        .to_string();
    drain_remaining_stderr(reader);

    (child, addr)
}

fn read_pty_until(fd: &impl AsFd, needle: &[u8], timeout: Duration) -> Vec<u8> {
    let started = Instant::now();
    let mut output = Vec::new();
    let mut buf = [0_u8; 256];
    while find_subslice(&output, needle).is_none() {
        assert!(started.elapsed() <= timeout, "PTY output timed out");
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
            Err(err) => panic!("read PTY output failed: {err}"),
        }
    }
    output
}

fn contains_ordered_bytes(haystack: &[u8], first: &[u8], second: &[u8]) -> bool {
    let Some(first_pos) = find_subslice(haystack, first) else {
        return false;
    };
    find_subslice(&haystack[first_pos + first.len()..], second).is_some()
}
