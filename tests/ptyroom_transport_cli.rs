mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{contains_bytes, drain_remaining_stderr, wait_child_stdout_until, wait_with_timeout};

#[test]
fn ptyroom_join_pipeline_receives_host_command_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared.ptytrace");
    let (host, addr) = spawn_ptyroom_host(&[
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
        "read line; printf 'shared:%s\\n' \"$line\"",
    ]);

    let mut join = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .arg("join")
        .arg(addr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    join.stdin
        .take()
        .unwrap()
        .write_all(b"hello from cli\n")
        .unwrap();

    let join_output = wait_with_timeout(join, Duration::from_secs(5));
    assert!(
        join_output.status.success(),
        "ptyroom join failed: {}",
        String::from_utf8_lossy(&join_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&join_output.stdout).contains("shared:hello from cli"),
        "ptyroom join stdout was {:?}",
        String::from_utf8_lossy(&join_output.stdout)
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
fn two_ptyroom_join_clients_both_receive_shared_command_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared-race.ptytrace");
    let (host, addr) = spawn_ptyroom_host(&[
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
        "read first; read second; printf 'seen:%s|%s\\n' \"$first\" \"$second\"",
    ]);

    let alpha = spawn_ptyroom_join_with_input(&addr, b"alpha\n");
    let omega = spawn_ptyroom_join_with_input(&addr, b"omega\n");

    let alpha_output = wait_with_timeout(alpha, Duration::from_secs(5));
    let omega_output = wait_with_timeout(omega, Duration::from_secs(5));
    assert_shared_output_mentions_both_inputs(&alpha_output);
    assert_shared_output_mentions_both_inputs(&omega_output);

    let host_output = wait_with_timeout(host, Duration::from_secs(5));
    assert!(
        host_output.status.success(),
        "ptyroom host failed: {}",
        String::from_utf8_lossy(&host_output.stderr)
    );
    assert!(trace_path.exists());
}

#[test]
fn ptyroom_late_join_decodes_replayed_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared-late.ptytrace");
    let (host, addr) = spawn_ptyroom_host(&[
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
        "printf 'ready\\n'; read line; printf 'late:%s\\n' \"$line\"",
    ]);
    let mut observer = spawn_ptyroom_join_with_input(&addr, b"");
    let ready_output = wait_child_stdout_until(&mut observer, b"ready", Duration::from_secs(5));
    assert!(
        contains_bytes(&ready_output, b"ready"),
        "observer stdout was {:?}",
        String::from_utf8_lossy(&ready_output)
    );
    let _ = observer.kill();
    let _ = observer.wait();

    let late = spawn_ptyroom_join_with_input(&addr, b"hello\n");
    let late_output = wait_with_timeout(late, Duration::from_secs(5));

    assert!(
        late_output.status.success(),
        "ptyroom join failed: {}",
        String::from_utf8_lossy(&late_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&late_output.stdout);
    assert!(
        stdout.contains("ready") && stdout.contains("late:hello"),
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
fn child_output_can_contain_ptyroom_control_lookalike() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared-spoof.ptytrace");
    let (host, addr) = spawn_ptyroom_host(&[
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
        "printf 'before\\033Pptyroom;size;1;1\\033\\\\after\\n'; sleep 0.5",
    ]);

    let observer = spawn_ptyroom_join_with_input(&addr, b"");
    let observer_output = wait_with_timeout(observer, Duration::from_secs(5));

    assert!(
        observer_output.status.success(),
        "ptyroom join failed: {}",
        String::from_utf8_lossy(&observer_output.stderr)
    );
    assert!(
        contains_bytes(
            &observer_output.stdout,
            b"before\x1bPptyroom;size;1;1\x1b\\after"
        ),
        "ptyroom join stdout was {:?}",
        String::from_utf8_lossy(&observer_output.stdout)
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
    let trace_path = tmp.path().join("shared-no-local-input.ptytrace");

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

fn spawn_ptyroom_host(args: &[&str]) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .arg("host")
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

fn spawn_ptyroom_join_with_input(addr: &str, input: &[u8]) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ptyroom"))
        .arg("join")
        .arg(addr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child
}

fn assert_shared_output_mentions_both_inputs(output: &Output) {
    assert!(
        output.status.success(),
        "ptyroom join failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("seen:") && stdout.contains("alpha") && stdout.contains("omega"),
        "ptyroom join stdout was {stdout:?}"
    );
}
