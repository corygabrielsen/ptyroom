use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[test]
fn termroom_join_receives_host_output() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("termroom.ptytrace");
    let (host, addr) = spawn_termroom_host(&[
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

    let mut join = Command::new(env!("CARGO_BIN_EXE_termroom"))
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
        "termroom join failed: {}",
        String::from_utf8_lossy(&join_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&join_output.stdout);
    assert!(
        stdout.contains("ready") && stdout.contains("room:hello"),
        "termroom join stdout was {stdout:?}"
    );

    let host_output = wait_with_timeout(host, Duration::from_secs(5));
    assert!(
        host_output.status.success(),
        "termroom host failed: {}",
        String::from_utf8_lossy(&host_output.stderr)
    );
    assert!(trace_path.exists());
}

fn spawn_termroom_host(args: &[&str]) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_termroom"))
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
        join_hint.contains("termroom join"),
        "missing termroom join hint: {join_hint:?}"
    );

    let addr = listening
        .trim()
        .strip_prefix("[termroom listening on ")
        .and_then(|line| line.strip_suffix(']'))
        .unwrap_or_else(|| panic!("unexpected termroom listening line: {listening:?}"))
        .to_string();

    (child, addr)
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
