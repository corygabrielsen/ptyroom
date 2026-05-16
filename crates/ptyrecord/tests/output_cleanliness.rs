//! Regression test for the "cargo run prompt bleeds into ptyrecord's
//! post-session output" class of bug.
//!
//! Mechanism the bug exploits: when a parent process (cargo's
//! compile-progress draw, an outer shell's prompt redraw, etc.) leaves
//! text on the terminal *without* a trailing newline, the cursor is
//! positioned somewhere on that row. ptyrecord's PTY child then runs.
//! When the child exits and our restore sequence runs, the cursor
//! returns to a position that may still have stale content to the right
//! of it. A bare `println!` from ptyrecord then overwrites only as far
//! as its own text reaches — the tail of whatever was already on that
//! row peeks through.
//!
//! This test reproduces the bug by writing a known sentinel string to
//! the slave terminal *before* spawning ptyrecord, then asserting that
//! after ptyrecord runs and we replay the entire master stream through
//! a VT100 emulator, no row in the final screen contains both a
//! "wrote " message and the sentinel. If they share a row, the bleed
//! is present.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const SENTINEL: &str = "STALE_PROMPT_NOT_CLEARED_BY_PTYRECORD";

/// Cols chosen so the sentinel + "wrote ..." messages both fit on one
/// row each — assertions stay deterministic.
const COLS: u16 = 200;
const ROWS: u16 = 40;

#[test]
fn wrote_messages_do_not_bleed_stale_terminal_content() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    // Spawn ptyrecord with a captured shell that reproduces the
    // class of terminal state the user's `cargo run` + zsh sees:
    //
    //   1. Print THREE sentinel lines on consecutive rows. These
    //      simulate any combination of cargo compile-progress lines,
    //      shell prompt redraws, or `[recording → ...]` banner that
    //      ptyrecord itself emits — content sitting on rows that
    //      will be on the primary screen when alt-screen exits.
    //   2. Move cursor UP 2 rows, then ENTER alt-screen at that
    //      mid-screen position. Real shells (zsh, bash with vi-mode,
    //      etc.) enter alt-screen wherever the cursor happened to
    //      be — not necessarily at a fresh row.
    //   3. Exit alt-screen explicitly (so we know the state at exit
    //      is deterministic). On exit, cursor restores to its
    //      pre-alt-screen position — mid-screen, with sentinel-bearing
    //      rows BELOW it that any subsequent printlns will land on
    //      unless they defensively clear each row.
    let cap_cmd = format!(
        "printf '{SENTINEL}_a\\n{SENTINEL}_b\\n{SENTINEL}_c\\n'; \
         printf '\\033[2A'; \
         printf '\\033[?1049h'; \
         printf 'inside alt screen'; \
         printf '\\033[?1049l'"
    );
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ptyrecord"));
    cmd.arg("/bin/sh");
    cmd.arg("-c");
    cmd.arg(&cap_cmd);
    cmd.cwd(std::env::temp_dir());
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    // Drain the master into a buffer on a worker thread; wait for the
    // child to exit with a timeout. portable-pty's master read blocks
    // until EOF, so we need the thread.
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

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("ptyrecord did not exit within 20s");
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("waiting for ptyrecord: {e}"),
        }
    }
    drop(pair.master);
    let _ = drain.join();

    let bytes = buf.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&bytes);

    // Always-on diagnostic dump so a failure leaves enough breadcrumbs
    // to debug from the failure message alone. Cargo only shows
    // captured stdout on test failure, so this costs nothing on pass.
    eprintln!("=== raw bytes from master ({} bytes) ===", bytes.len());
    eprintln!("{}", text.escape_debug());
    eprintln!("=== end raw bytes ===");

    // Replay the full master stream through a VT100 emulator. The
    // result is what a human user would see on their terminal after
    // ptyrecord exits. This is the right surface to assert on — raw
    // byte inspection misses things like cursor positioning + clears,
    // which is exactly what we're trying to verify.
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    parser.process(&bytes);
    let screen = parser.screen();

    let rows: Vec<String> = screen.rows(0, ROWS).collect();

    eprintln!("=== rendered screen (after vt100) ===");
    eprintln!("{}", screen_dump(screen));
    eprintln!("=== end screen ===");

    let mut wrote_rows: Vec<(usize, String)> = Vec::new();
    for (row_idx, line) in rows.iter().enumerate() {
        let line = line.trim_end().to_string();
        if line.contains("wrote ") {
            wrote_rows.push((row_idx, line));
        }
    }

    assert!(
        !wrote_rows.is_empty(),
        "expected at least one 'wrote ' message in ptyrecord's output. \
         raw bytes: {text:?}"
    );

    // Strict shape: each "wrote " row, after the announced path's
    // extension, must contain only whitespace. ANY non-whitespace
    // tail is bleed from a row whose pre-existing content survived
    // a non-clearing println.
    let known_extensions = [".ptyrecord", ".mp4", ".ptytrace"];
    for (row, line) in &wrote_rows {
        let (_, after_wrote) = line.split_once("wrote ").unwrap();
        let Some(ext_end) = known_extensions
            .iter()
            .filter_map(|ext| after_wrote.find(ext).map(|i| i + ext.len()))
            .min()
        else {
            panic!("'wrote' row {row} has no known artifact extension: {line:?}");
        };
        let tail = &after_wrote[ext_end..];
        assert!(
            tail.trim().is_empty(),
            "'wrote' row {row} has bleed-through tail past the path's extension:\n\
             row: {line:?}\n\
             tail (everything after the artifact extension): {tail:?}\n\
             full screen:\n{}",
            screen_dump(screen),
        );
    }
}

fn screen_dump(screen: &vt100::Screen) -> String {
    let mut out = String::new();
    for line in screen.rows(0, ROWS) {
        out.push_str("  | ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
