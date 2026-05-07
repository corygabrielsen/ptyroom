mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{contains_bytes, drain_remaining_stderr, wait_child_stdout_until, wait_with_timeout};

#[test]
fn ptyconnect_pipeline_receives_ptyshare_command_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared.ptytrace");
    let (share, addr) = spawn_ptyshare(&[
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

    let mut connect = Command::new(env!("CARGO_BIN_EXE_ptyconnect"))
        .arg(addr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    connect
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello from cli\n")
        .unwrap();

    let connect_output = wait_with_timeout(connect, Duration::from_secs(5));
    assert!(
        connect_output.status.success(),
        "ptyconnect failed: {}",
        String::from_utf8_lossy(&connect_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&connect_output.stdout).contains("shared:hello from cli"),
        "ptyconnect stdout was {:?}",
        String::from_utf8_lossy(&connect_output.stdout)
    );

    let share_output = wait_with_timeout(share, Duration::from_secs(5));
    assert!(
        share_output.status.success(),
        "ptyshare failed: {}",
        String::from_utf8_lossy(&share_output.stderr)
    );
    assert!(trace_path.exists());
}

#[test]
fn two_ptyconnect_clients_both_receive_shared_command_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared-race.ptytrace");
    let (share, addr) = spawn_ptyshare(&[
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

    let alpha = spawn_ptyconnect_with_input(&addr, b"alpha\n");
    let omega = spawn_ptyconnect_with_input(&addr, b"omega\n");

    let alpha_output = wait_with_timeout(alpha, Duration::from_secs(5));
    let omega_output = wait_with_timeout(omega, Duration::from_secs(5));
    assert_shared_output_mentions_both_inputs(&alpha_output);
    assert_shared_output_mentions_both_inputs(&omega_output);

    let share_output = wait_with_timeout(share, Duration::from_secs(5));
    assert!(
        share_output.status.success(),
        "ptyshare failed: {}",
        String::from_utf8_lossy(&share_output.stderr)
    );
    assert!(trace_path.exists());
}

#[test]
fn ptyconnect_late_join_decodes_replayed_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared-late.ptytrace");
    let (share, addr) = spawn_ptyshare(&[
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
    let mut observer = spawn_ptyconnect_with_input(&addr, b"");
    let ready_output = wait_child_stdout_until(&mut observer, b"ready", Duration::from_secs(5));
    assert!(
        contains_bytes(&ready_output, b"ready"),
        "observer stdout was {:?}",
        String::from_utf8_lossy(&ready_output)
    );
    let _ = observer.kill();
    let _ = observer.wait();

    let late = spawn_ptyconnect_with_input(&addr, b"hello\n");
    let late_output = wait_with_timeout(late, Duration::from_secs(5));

    assert!(
        late_output.status.success(),
        "ptyconnect failed: {}",
        String::from_utf8_lossy(&late_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&late_output.stdout);
    assert!(
        stdout.contains("ready") && stdout.contains("late:hello"),
        "ptyconnect stdout was {stdout:?}"
    );
    let share_output = wait_with_timeout(share, Duration::from_secs(5));
    assert!(
        share_output.status.success(),
        "ptyshare failed: {}",
        String::from_utf8_lossy(&share_output.stderr)
    );
    assert!(trace_path.exists());
}

#[test]
fn child_output_can_contain_ptyshare_control_lookalike() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared-spoof.ptytrace");
    let (share, addr) = spawn_ptyshare(&[
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
        "printf 'before\\033Pptyshare;size;1;1\\033\\\\after\\n'; sleep 0.5",
    ]);

    let observer = spawn_ptyconnect_with_input(&addr, b"");
    let observer_output = wait_with_timeout(observer, Duration::from_secs(5));

    assert!(
        observer_output.status.success(),
        "ptyconnect failed: {}",
        String::from_utf8_lossy(&observer_output.stderr)
    );
    assert!(
        contains_bytes(
            &observer_output.stdout,
            b"before\x1bPptyshare;size;1;1\x1b\\after"
        ),
        "ptyconnect stdout was {:?}",
        String::from_utf8_lossy(&observer_output.stdout)
    );
    let share_output = wait_with_timeout(share, Duration::from_secs(5));
    assert!(
        share_output.status.success(),
        "ptyshare failed: {}",
        String::from_utf8_lossy(&share_output.stderr)
    );
    assert!(trace_path.exists());
}

#[test]
fn ptyshare_warns_when_local_input_is_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("shared-no-local-input.ptytrace");

    let output = Command::new(env!("CARGO_BIN_EXE_ptyshare"))
        .args([
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
        "ptyshare failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("host input disabled"),
        "ptyshare stderr was {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn spawn_ptyshare(args: &[&str]) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ptyshare"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);
    let mut listening = String::new();
    let mut connect_hint = String::new();
    reader.read_line(&mut listening).unwrap();
    reader.read_line(&mut connect_hint).unwrap();
    assert!(
        connect_hint.contains("ptyconnect"),
        "missing ptyconnect hint: {connect_hint:?}"
    );

    let addr = listening
        .trim()
        .strip_prefix("[ptyshare listening on ")
        .and_then(|line| line.strip_suffix(']'))
        .unwrap_or_else(|| panic!("unexpected ptyshare listening line: {listening:?}"))
        .to_string();
    drain_remaining_stderr(reader);

    (child, addr)
}

fn spawn_ptyconnect_with_input(addr: &str, input: &[u8]) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ptyconnect"))
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
        "ptyconnect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("seen:") && stdout.contains("alpha") && stdout.contains("omega"),
        "ptyconnect stdout was {stdout:?}"
    );
}
