//! Regression tests for ptyrecord's post-session terminal output.
//!
//! The bug class: after a PTY-captured session, the terminal can be in
//! arbitrary state — cursor anywhere on or below pre-session content,
//! alt-screen residue, scrolled or resized mid-session. A naive
//! `println!("wrote ...")` lands wherever the cursor happens to be, on
//! top of whatever pre-existing content was on that row, overwriting
//! only as far as the println's bytes reach. The visible result: stale
//! row tail bleeds past the println text.
//!
//! Defense: before any post-session println, scroll the prior visible
//! content into scrollback (by emitting 2 × rows newlines) and home
//! the cursor on the now-blank viewport. This is resilient to:
//!   - any starting cursor position
//!   - any terminal width (the wrap-around scenario the user flagged)
//!   - terminals that were resized mid-session (we re-detect rows
//!     after the captured session ends)
//!
//! Each test in this file spawns ptyrecord under a portable-pty
//! session, runs a captured shell that engineers a specific
//! pre-existing terminal state, then replays the master-side bytes
//! through a vt100 emulator at the same dimensions to inspect what
//! the user would see. Any "wrote" row whose tail past the announced
//! artifact extension contains non-whitespace is a bleed regression
//! and fails with a screen dump.

use std::fmt::Write as _;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const SENTINEL: &str = "STALE_PROMPT_NOT_CLEARED_BY_PTYRECORD";

/// Single `#[test]` that loops through every scenario serially.
/// Splitting into separate `#[test]` functions flakes under cargo's
/// default test parallelism — each scenario spawns ptyrecord (which
/// spawns ffmpeg), and concurrent PTY/ffmpeg processes contend in
/// ways that surface as spurious bleed-test failures. Serial run
/// keeps each scenario's terminal state isolated.
#[test]
fn no_bleed_across_terminal_scenarios() {
    // Width 40 so the "wrote ..." messages (37-38 chars) don't wrap
    // by themselves but sentinel content does — exercises the
    // wrapped-row branch of the substrate prep.
    let mut scenarios: Vec<(&str, Scenario)> = vec![
        ("standard_80x24", alt_screen_reproducer_at(80, 24)),
        ("narrow_40x24", alt_screen_reproducer_at(40, 24)),
        ("wide_200x24", alt_screen_reproducer_at(200, 24)),
        ("resized_mid_session", {
            let mut s = alt_screen_reproducer_at(80, 24);
            s.resize_to = Some((50, 30));
            s
        }),
    ];
    // Captured shell that emits 80 sentinel lines — exercises the
    // primary-screen-scrolled case (no alt-screen, content piles up
    // and scrolls past the viewport).
    scenarios.push((
        "scrolling_primary_screen",
        Scenario {
            initial_cols: 80,
            initial_rows: 24,
            resize_to: None,
            captured_cmd: (1..=80)
                .map(|i| format!("printf '{SENTINEL}_scroll_line_{i:03}\\n'"))
                .collect::<Vec<_>>()
                .join("; "),
        },
    ));

    for (name, scenario) in scenarios {
        eprintln!("\n\n########## scenario: {name} ##########");
        assert_no_bleed_scenario(scenario);
    }
}

struct Scenario {
    initial_cols: u16,
    initial_rows: u16,
    resize_to: Option<(u16, u16)>,
    captured_cmd: String,
}

fn alt_screen_reproducer_at(cols: u16, rows: u16) -> Scenario {
    Scenario {
        initial_cols: cols,
        initial_rows: rows,
        resize_to: None,
        // 3 sentinel rows, cursor up 2, enter/exit alt-screen at the
        // mid-screen position. On exit, cursor restores to a row that
        // has a sentinel BELOW it — any subsequent println without
        // substrate prep lands on the sentinel-bearing row.
        captured_cmd: format!(
            "printf '{SENTINEL}_a\\n{SENTINEL}_b\\n{SENTINEL}_c\\n'; \
             printf '\\033[2A'; \
             printf '\\033[?1049h'; \
             printf 'inside alt screen'; \
             printf '\\033[?1049l'"
        ),
    }
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn assert_no_bleed_scenario(scenario: Scenario) {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: scenario.initial_rows,
            cols: scenario.initial_cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ptyrecord"));
    cmd.arg("/bin/sh");
    cmd.arg("-c");
    cmd.arg(&scenario.captured_cmd);
    cmd.cwd(std::env::temp_dir());
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    if let Some((cols, rows)) = scenario.resize_to {
        // Brief sleep to give the captured shell a moment to begin
        // execution before we resize. Without this the resize can
        // race the shell's first byte and the test becomes flaky on
        // slow systems.
        thread::sleep(Duration::from_millis(50));
        pair.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
    }

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

    // Render at the FINAL size (after any resize). This is what the
    // user would see on their terminal after ptyrecord exited.
    let (render_cols, render_rows) = scenario
        .resize_to
        .unwrap_or((scenario.initial_cols, scenario.initial_rows));
    let mut parser = vt100::Parser::new(render_rows, render_cols, 0);
    parser.process(&bytes);
    let screen = parser.screen();

    eprintln!(
        "=== scenario: cols={} rows={}{} ===",
        render_cols,
        render_rows,
        scenario
            .resize_to
            .map(|_| format!(
                " (resized from {}x{})",
                scenario.initial_cols, scenario.initial_rows
            ))
            .unwrap_or_default(),
    );
    eprintln!("=== raw bytes ({} bytes) ===", bytes.len());
    eprintln!("{}", text.escape_debug());
    eprintln!("=== rendered screen ===");
    eprintln!("{}", screen_dump(screen, render_cols));

    let rows: Vec<String> = screen.rows(0, render_cols).collect();
    let mut wrote_rows: Vec<(usize, String)> = Vec::new();
    for (row_idx, line) in rows.iter().enumerate() {
        let trimmed = line.trim_end().to_string();
        if trimmed.contains("wrote ") {
            wrote_rows.push((row_idx, trimmed));
        }
    }

    assert!(
        !wrote_rows.is_empty(),
        "expected at least one 'wrote ' message in ptyrecord's output. \
         raw bytes: {text:?}"
    );

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
            screen_dump(screen, render_cols),
        );
    }
}

fn screen_dump(screen: &vt100::Screen, cols: u16) -> String {
    let mut out = String::new();
    for (i, line) in screen.rows(0, cols).enumerate() {
        let _ = write!(out, "  {i:2} | ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
