//! Invariant-driven tests for ptyrecord.
//!
//! Each test function name maps directly to a named invariant in
//! `INVARIANTS.md` (sibling of this crate's `Cargo.toml`). When an
//! invariant changes, update the doc first, then this file, then the
//! code. When a future bug report comes in, look up which invariant
//! it violates here — the source comments referencing that invariant
//! point at the code responsible.
//!
//! All assertions run sequentially via a single top-level `#[test]`
//! that drives multiple scenarios. Each spawns ptyrecord as a real
//! child process (via `portable-pty` for the tty-shaped cases, via
//! plain `Command` for the piped-stdout case). cargo's default test
//! parallelism contends on PTY/ffmpeg resources and produces
//! spurious failures; serial drive is the price of end-to-end
//! fidelity.

use std::fmt::Write as _;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Run every invariant check serially. Add new invariants here.
///
/// Some tests need fixture artifact files (a real `.ptytrace` and
/// `.mp4` to feed into ptyrecord's `--trace-in` mode for the piped-
/// stdout checks). We generate those once at the top by running
/// ptyrecord under a real PTY with `--trace-out`/`--media-out`.
#[test]
fn ptyrecord_invariants() {
    let fixtures = Fixtures::generate();

    captured_session_in_alt_screen();
    contract_files_exist();
    piped_stdout_is_plain(&fixtures);
    no_screen_clear_sequences();
    scrollback_preserved();
    notification_best_effort_emits_lines(&fixtures);
}

/// Pre-generated `.ptytrace` + `.mp4` files for tests that need to
/// invoke ptyrecord in `--trace-in` mode (which doesn't require a
/// tty on stdin and can therefore be piped end-to-end).
struct Fixtures {
    _tmp: tempfile::TempDir,
    trace: std::path::PathBuf,
    media: std::path::PathBuf,
}

impl Fixtures {
    fn generate() -> Self {
        eprintln!("\n=== generating shared fixtures ===");
        let tmp = tempfile::tempdir().unwrap();
        let trace = tmp.path().join("fixture.ptytrace");
        let media = tmp.path().join("fixture.mp4");
        let bundle = tmp.path().join("fixture.ptyrecord");

        let pair = open_pty(80, 24);
        let mut cb = CommandBuilder::new(env!("CARGO_BIN_EXE_ptyrecord"));
        cb.args([
            "--out".as_ref(),
            bundle.as_os_str(),
            "--trace-out".as_ref(),
            trace.as_os_str(),
            "--media-out".as_ref(),
            media.as_os_str(),
            "--".as_ref(),
            "/bin/sh".as_ref(),
            "-c".as_ref(),
            "printf hi".as_ref(),
        ]);
        cb.cwd(std::env::temp_dir());
        let mut child = pair.slave.spawn_command(cb).unwrap();
        // Drop slave so master EOFs when child exits (otherwise the
        // drain thread blocks forever).
        drop(pair.slave);
        // Drain master so the pty buffer doesn't fill and block.
        let mut reader = pair.master.try_clone_reader().unwrap();
        let drain = thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink);
        });
        wait_with_timeout(&mut child, Duration::from_secs(20));
        drop(pair.master);
        let _ = drain.join();

        assert!(trace.exists(), "fixture trace not produced");
        assert!(media.exists(), "fixture media not produced");
        Self {
            _tmp: tmp,
            trace,
            media,
        }
    }
}

// =====================================================================
// INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN
// =====================================================================

/// `INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN`: ptyrecord enters the
/// xterm alternate screen buffer before running the captured PTY and
/// the captured session's bytes are emitted while we are in that
/// buffer. On exit the restore sequence flips back to the primary
/// buffer.
///
/// Detection: spawn ptyrecord under a controlled PTY, run a
/// captured shell that emits a unique sentinel byte. In the output
/// stream, the sentinel must appear AFTER `\x1b[?1049h` and BEFORE
/// the next `\x1b[?1049l`.
fn captured_session_in_alt_screen() {
    eprintln!("\n=== INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN ===");
    let pair = open_pty(80, 24);
    let bytes = run_ptyrecord_under_pty(pair, "printf '@CAPTURED@'");

    let enter = b"\x1b[?1049h";
    let exit = b"\x1b[?1049l";
    let sentinel = b"@CAPTURED@";

    let enter_pos = find_subslice(&bytes, enter).unwrap_or_else(|| {
        panic!(
            "alt-screen enter (`\\x1b[?1049h`) never appeared in the output. \
             violates INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN.\n\
             full stream: {}",
            String::from_utf8_lossy(&bytes).escape_debug(),
        )
    });
    let sentinel_pos = find_subslice(&bytes, sentinel).unwrap_or_else(|| {
        panic!(
            "captured-session sentinel never appeared in the output. \
             full stream: {}",
            String::from_utf8_lossy(&bytes).escape_debug(),
        )
    });
    // The exit MUST come after the sentinel — i.e. we don't leave
    // alt-screen until after the captured session has finished.
    let exit_after_sentinel = find_subslice(&bytes[sentinel_pos..], exit).map_or_else(
        || {
            panic!(
                "alt-screen exit (`\\x1b[?1049l`) never appeared after the \
                 captured-session sentinel. \
                 violates INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN.\n\
                 full stream: {}",
                String::from_utf8_lossy(&bytes).escape_debug(),
            )
        },
        |p| p + sentinel_pos,
    );

    assert!(
        enter_pos < sentinel_pos && sentinel_pos < exit_after_sentinel,
        "expected ordering: alt-screen enter ({enter_pos}) < sentinel \
         ({sentinel_pos}) < alt-screen exit ({exit_after_sentinel}). \
         violates INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN.\n\
         full stream: {}",
        String::from_utf8_lossy(&bytes).escape_debug(),
    );

    // Cursor must be HOMED on alt-screen entry. xterm's `\x1b[?1049h`
    // doesn't reset cursor position — the saved-from-primary
    // position carries over. Without an explicit `\x1b[H`, the
    // captured shell's first prompt draws wherever the user's
    // prompt happened to be on primary, typically halfway down the
    // screen. Replay the alt-screen-enter bytes through vt100
    // INTO A NON-EMPTY STARTING STATE (cursor pre-positioned
    // mid-screen, like a real terminal) and assert the cursor is
    // at (0, 0) afterwards.
    //
    // Naming this assertion separately so a future failure points
    // exactly at "you forgot the cursor home" rather than
    // "ordering wrong somewhere."
    let mut parser = vt100::Parser::new(40, 200, 0);
    // Pre-position cursor at (15, 30) — simulates being mid-screen
    // when the user runs ptyrecord. xterm's 1049 saves THIS
    // position. The fix's `\x1b[H` must override it.
    parser.process(b"\x1b[16;31H");
    let (pre_row, pre_col) = parser.screen().cursor_position();
    assert_eq!(
        (pre_row, pre_col),
        (15, 30),
        "vt100 setup failure: cursor not where the test pre-positioned it",
    );
    // Feed only the bytes UP TO and INCLUDING alt-screen enter,
    // stopping just before the captured session writes anything.
    // That isolates "what does the substrate look like right when
    // the captured shell starts?"
    parser.process(&bytes[..sentinel_pos]);
    let (row, col) = parser.screen().cursor_position();
    assert_eq!(
        (row, col),
        (0, 0),
        "after alt-screen entry, cursor should be at (0, 0) so the \
         captured shell's first prompt lands at top-left. Got ({row}, \
         {col}). violates INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN.\n\
         enter-prelude bytes (escaped): {}",
        String::from_utf8_lossy(&bytes[..sentinel_pos]).escape_debug(),
    );
}

// =====================================================================
// INVARIANT_CONTRACT_FILES_EXIST
// =====================================================================

/// `INVARIANT_CONTRACT_FILES_EXIST`: after `ptyrecord` exits 0, the
/// announced bundle file exists on disk.
///
/// Drives ptyrecord via the PTY path (real live capture) so this
/// also covers the common interactive case end-to-end. The bytes
/// from the master include both the captured shell's output and
/// ptyrecord's own — we just check the file appeared on disk.
fn contract_files_exist() {
    eprintln!("\n=== INVARIANT_CONTRACT_FILES_EXIST ===");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("session.ptyrecord");
    let media = tmp.path().join("session.mp4");

    let pair = open_pty(80, 24);
    let mut cb = CommandBuilder::new(env!("CARGO_BIN_EXE_ptyrecord"));
    cb.args([
        "--out".as_ref(),
        bundle.as_os_str(),
        "--".as_ref(),
        "/bin/sh".as_ref(),
        "-c".as_ref(),
        "printf hi".as_ref(),
    ]);
    cb.cwd(std::env::temp_dir());
    let mut child = pair.slave.spawn_command(cb).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let drain = thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = reader.read_to_end(&mut sink);
    });
    wait_with_timeout(&mut child, Duration::from_secs(20));
    drop(pair.master);
    let _ = drain.join();

    assert!(
        bundle.exists(),
        "bundle path does not exist after ptyrecord exit: {}",
        bundle.display(),
    );
    assert!(
        media.exists(),
        "media sidecar does not exist after ptyrecord exit: {}",
        media.display(),
    );
}

// =====================================================================
// INVARIANT_PIPED_STDOUT_IS_PLAIN
// =====================================================================

/// `INVARIANT_PIPED_STDOUT_IS_PLAIN`: when stdout is not a tty, no
/// ANSI escape sequences appear in stdout.
///
/// Uses `--trace-in` mode (no PTY required) so we can drive ptyrecord
/// with fully piped stdin/stdout/stderr and inspect the raw bytes.
fn piped_stdout_is_plain(fixtures: &Fixtures) {
    eprintln!("\n=== INVARIANT_PIPED_STDOUT_IS_PLAIN ===");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("repackaged.ptyrecord");

    let output = run_ptyrecord_piped(&[
        "--out".as_ref(),
        bundle.as_os_str(),
        "--trace-in".as_ref(),
        fixtures.trace.as_os_str(),
        "--media-in".as_ref(),
        fixtures.media.as_os_str(),
    ]);
    assert!(
        output.status.success(),
        "ptyrecord --trace-in exited non-zero: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        !output.stdout.contains(&0x1b),
        "stdout contained an ESC (0x1b) byte when stdout was a pipe — \
         violates INVARIANT_PIPED_STDOUT_IS_PLAIN.\n\
         stdout (escaped): {}",
        String::from_utf8_lossy(&output.stdout).escape_debug(),
    );
}

// =====================================================================
// INVARIANT_USER_TERMINAL_NOT_CLEARED
// =====================================================================

/// `INVARIANT_USER_TERMINAL_NOT_CLEARED`: ptyrecord does not emit
/// screen-clearing control sequences on its own initiative.
///
/// Enforcement: spawn ptyrecord with a captured shell that exits
/// immediately without writing anything. Anything in the output
/// stream is therefore ptyrecord-originated. Scan for forbidden
/// sequences. Per-row clear (`\x1b[2K\r`) is explicitly allowed —
/// see the invariant's doc for why.
fn no_screen_clear_sequences() {
    eprintln!("\n=== INVARIANT_USER_TERMINAL_NOT_CLEARED ===");
    let pair = open_pty(80, 24);
    let bytes = run_ptyrecord_under_pty(pair, "true");

    let forbidden: &[(&str, &[u8])] = &[
        ("CSI 2 J (erase entire screen)", b"\x1b[2J"),
        ("CSI 3 J (erase scrollback)", b"\x1b[3J"),
        ("ESC c (RIS reset)", b"\x1bc"),
    ];
    for (name, seq) in forbidden {
        assert!(
            find_subslice(&bytes, seq).is_none(),
            "ptyrecord emitted forbidden screen-clear sequence '{name}'. \
             violates INVARIANT_USER_TERMINAL_NOT_CLEARED.\n\
             full stream (escaped): {}",
            String::from_utf8_lossy(&bytes).escape_debug(),
        );
    }
}

// =====================================================================
// INVARIANT_USER_SCROLLBACK_PRESERVED
// =====================================================================

/// `INVARIANT_USER_SCROLLBACK_PRESERVED`: ptyrecord does not push
/// prior visible content out of the user's viewport via padding
/// newlines.
///
/// Detection: count newlines that ptyrecord adds AFTER the captured
/// session's last byte and BEFORE the first `wrote` message. The
/// captured session's own newlines are excluded. A small number of
/// structural newlines is acceptable; a large number (multiples of
/// terminal rows, as commit `208ad80` did with `2 × rows` padding)
/// is a violation.
fn scrollback_preserved() {
    // Threshold: 2 newlines is enough for "advance past captured
    // line + one blank for breathing room." The destructive fix
    // emitted 48 newlines (2 × 24 rows); the threshold catches that
    // with plenty of margin. If a future change genuinely needs more
    // newlines, raise this threshold WITH the justification recorded
    // in INVARIANTS.md.
    const MAX_PREAMBLE_NEWLINES: usize = 2;

    eprintln!("\n=== INVARIANT_USER_SCROLLBACK_PRESERVED ===");
    let pair = open_pty(80, 24);
    // Captured shell emits a single non-newline byte then exits. The
    // byte makes the session non-empty (so the bundle gets built)
    // but minimizes the captured-session bytes we'd otherwise have
    // to filter out.
    let bytes = run_ptyrecord_under_pty(pair, "printf x");

    let x_pos = bytes
        .iter()
        .position(|&b| b == b'x')
        .expect("captured 'x' byte not found in output stream");
    let wrote_pos = find_subslice(&bytes[x_pos..], b"wrote ")
        .map(|p| p + x_pos)
        .expect("'wrote' marker not found in output stream");

    let preamble = &bytes[x_pos + 1..wrote_pos];
    #[allow(clippy::naive_bytecount)]
    let newline_count = preamble.iter().filter(|&&b| b == b'\n').count();
    assert!(
        newline_count <= MAX_PREAMBLE_NEWLINES,
        "ptyrecord emitted {newline_count} padding newlines after the \
         captured session before the first 'wrote' message — max \
         allowed is {MAX_PREAMBLE_NEWLINES}. \
         violates INVARIANT_USER_SCROLLBACK_PRESERVED.\n\
         preamble bytes (escaped): {}",
        String::from_utf8_lossy(preamble).escape_debug(),
    );
}

// =====================================================================
// INVARIANT_NOTIFICATION_BEST_EFFORT
// =====================================================================

/// `INVARIANT_NOTIFICATION_BEST_EFFORT`: ptyrecord notifies the user
/// of the artifact it produced. This is a SOFT invariant — we
/// promise the path is mentioned in stdout, not that the rendering
/// is visually perfect under all terminal states.
///
/// Uses `--trace-in` mode (piped) so the assertion can read stdout
/// directly without PTY interleaving.
fn notification_best_effort_emits_lines(fixtures: &Fixtures) {
    eprintln!("\n=== INVARIANT_NOTIFICATION_BEST_EFFORT ===");
    let tmp = tempfile::tempdir().unwrap();
    let bundle = tmp.path().join("notify.ptyrecord");

    let output = run_ptyrecord_piped(&[
        "--out".as_ref(),
        bundle.as_os_str(),
        "--trace-in".as_ref(),
        fixtures.trace.as_os_str(),
        "--media-in".as_ref(),
        fixtures.media.as_os_str(),
    ]);
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("wrote "),
        "stdout missing 'wrote ' notification. \
         violates INVARIANT_NOTIFICATION_BEST_EFFORT.\n\
         stdout: {stdout:?}",
    );
    assert!(
        stdout.contains(&bundle.display().to_string()),
        "stdout missing the bundle path. \
         violates INVARIANT_NOTIFICATION_BEST_EFFORT.\n\
         stdout: {stdout:?}",
    );
}

// =====================================================================
// helpers
// =====================================================================

fn open_pty(cols: u16, rows: u16) -> portable_pty::PtyPair {
    native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap()
}

/// Spawn ptyrecord under `pair`'s slave with a captured shell that
/// runs `cmd`. Drain all bytes from the master, return the full
/// stream. Times out at 20s.
///
/// Takes ownership of `pair` because we need to drop the slave after
/// spawning the child (so the master sees EOF when the child exits
/// — without this, the drain thread hangs forever waiting for bytes
/// on a slave fd that nobody else writes to).
fn run_ptyrecord_under_pty(pair: portable_pty::PtyPair, cmd: &str) -> Vec<u8> {
    let mut cb = CommandBuilder::new(env!("CARGO_BIN_EXE_ptyrecord"));
    cb.arg("/bin/sh");
    cb.arg("-c");
    cb.arg(cmd);
    cb.cwd(std::env::temp_dir());
    let mut child = pair.slave.spawn_command(cb).unwrap();
    // Critical: drop the slave so the master EOFs when the child
    // exits. Without this the drain thread blocks indefinitely.
    drop(pair.slave);

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_writer = Arc::clone(&buf);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let drain = thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_writer.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });

    wait_with_timeout(&mut child, Duration::from_secs(20));
    // Drop the master to be doubly sure the drain thread unblocks
    // (the cloned reader gets EOF when the underlying master fd is
    // closed by every holder).
    drop(pair.master);
    let _ = drain.join();
    let v = buf.lock().unwrap().clone();
    eprintln!(
        "  captured {} bytes from master (escaped, truncated): {}",
        v.len(),
        truncate(&String::from_utf8_lossy(&v).escape_debug().to_string(), 400),
    );
    v
}

/// Spawn ptyrecord with piped stdin/stdout/stderr (no tty). Returns
/// the std `Output` for the caller to inspect.
fn run_ptyrecord_piped(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ptyrecord"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

/// Poll-wait for a portable-pty child with a timeout. Kills the
/// child on expiry.
fn wait_with_timeout(child: &mut Box<dyn portable_pty::Child + Send + Sync>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("child did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= max_chars {
            let _ = write!(out, "… ({} more)", s.chars().count() - max_chars);
            break;
        }
        out.push(c);
    }
    out
}
