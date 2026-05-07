use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

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

fn wait_with_timeout(mut child: Child, timeout: Duration) -> Output {
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
