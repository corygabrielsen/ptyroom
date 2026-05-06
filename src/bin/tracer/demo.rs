//! `tracer` (bare) — the one-command demo flow.
//!
//! No subcommand. No flags. Capture a live terminal session, render
//! it to a GIF, produce a reproducibility witness, open the GIF, and
//! print a hash summary. Everything end-to-end in a single command.
//!
//! Designed for live demos: the audience watches a terminal session,
//! and at the end a video pops up plus a JSON witness whose hashes
//! anyone can re-verify on any machine.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tracer::tracer::{CaptureOpts, capture};

const BASENAME: &str = "demo";
const FONT_SIZE: f32 = 14.0;
const GIF_WIDTH: u32 = 800;

pub fn run() -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "tracer: stdin is not a terminal — bare-mode demo needs an interactive tty.\n\
             Pipe-driven users want `tracer capture --out X` (writes a trace) or\n\
             `tracer render <trace> <out>` (renders an existing trace)."
        );
    }

    let trace_path = PathBuf::from(format!("{BASENAME}.trace"));
    let gif_path = PathBuf::from(format!("{BASENAME}.gif"));
    let witness_path = PathBuf::from(format!("{BASENAME}.witness.json"));

    eprintln!();
    eprintln!("─── tracer ─────────────────────────────────────────");
    eprintln!("recording → {}", trace_path.display());
    eprintln!("type freely; Ctrl-D or `exit` to finish");
    eprintln!("────────────────────────────────────────────────────");
    eprintln!();

    // 1. Capture the live session.
    let trace = capture(CaptureOpts {
        argv: Vec::new(),
        cols: None,
        rows: None,
        // 1 hour cap. `Duration::from_mins`/`from_hours` are unstable;
        // use seconds and silence the larger-unit lint.
        #[allow(clippy::duration_suboptimal_units)]
        max_runtime: Duration::from_secs(3600),
    })?;
    let event_count = trace.events.len();
    let duration_s = trace.events.last().map_or(0.0, |e| e.time_s);
    trace.write(&trace_path)?;

    eprintln!();
    eprintln!("─── post-capture ───────────────────────────────────");

    // 2. Render to GIF + emit witness.
    let witness = tracer::render(&trace_path)?
        .font_size(FONT_SIZE)
        .width(GIF_WIDTH)
        .to_path_with_receipt(&gif_path)?;
    witness.write(&witness_path)?;

    let trace_size = std::fs::metadata(&trace_path).map_or(0, |m| m.len());
    let gif_size = std::fs::metadata(&gif_path).map_or(0, |m| m.len());

    eprintln!(
        "✓ trace      {}  ({} events, {:.1}s, {})",
        trace_path.display(),
        event_count,
        duration_s,
        fmt_size(trace_size),
    );
    eprintln!(
        "✓ rendered   {}  ({})",
        gif_path.display(),
        fmt_size(gif_size),
    );
    eprintln!("✓ witness    {}", witness_path.display());
    eprintln!(
        "    trace_sha256:    {}…",
        short_hash(&witness.trace_sha256)
    );
    eprintln!(
        "    output_sha256:   {}…",
        short_hash(&witness.output_sha256)
    );
    if let Some(rec_sha) = &witness.tool.recorder_sha256 {
        eprintln!("    tracer_sha256:   {}…", short_hash(rec_sha));
    }
    if let Some(ff_sha) = &witness.tool.ffmpeg_sha256 {
        eprintln!("    ffmpeg_sha256:   {}…", short_hash(ff_sha));
    }

    eprintln!();
    eprintln!("─── verify on any machine ──────────────────────────");
    eprintln!(
        "    tracer verify --witness {} --trace {}",
        witness_path.display(),
        trace_path.display(),
    );
    eprintln!();

    // 3. Try to open the GIF.
    match open_path(&gif_path) {
        Ok(opener) => eprintln!("opened {} (via {})", gif_path.display(), opener),
        Err(e) => {
            eprintln!("(could not auto-open: {e})");
            if let Ok(canon) = gif_path.canonicalize() {
                eprintln!("open manually: file://{}", canon.display());
            }
        }
    }
    eprintln!();

    Ok(())
}

/// Spawn a platform-appropriate opener and return its name.
///
/// On WSL prefer `wslview` (forks via Windows shell, never hangs);
/// elsewhere try `xdg-open`, then macOS `open`, with `explorer.exe`
/// and `wslview` as fallbacks. We `spawn()` rather than `status()`
/// so a graphical viewer that takes a moment to come up doesn't
/// block the demo from finishing — if the opener crashes the user
/// still has the GIF path printed above.
fn open_path(path: &Path) -> anyhow::Result<&'static str> {
    let on_wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
    let openers: &[&str] = if on_wsl {
        &["wslview", "explorer.exe", "xdg-open"]
    } else {
        &["xdg-open", "open", "wslview"]
    };
    for opener in openers {
        let spawn = Command::new(opener)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawn.is_ok() {
            return Ok(opener);
        }
    }
    anyhow::bail!("no opener available (tried: {})", openers.join(", "))
}

fn fmt_size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn short_hash(hash: &str) -> &str {
    &hash[..hash.len().min(16)]
}
