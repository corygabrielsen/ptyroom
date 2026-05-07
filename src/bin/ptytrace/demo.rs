//! `ptytrace` (bare) — the one-command demo flow.
//!
//! No subcommand. No flags. Capture a live terminal session, render
//! it to a GIF, produce a reproducibility witness plus attestation,
//! open the GIF, and print a hash summary. Everything end-to-end in a
//! single command.
//!
//! Designed for live demos: the audience watches a terminal session,
//! and at the end a video pops up plus JSON sidecars whose hashes
//! anyone can re-verify on any machine.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ptytrace::pty::{CaptureOpts, capture};
use ptytrace::witness::VerifyOutcome;

const BASENAME: &str = "demo";
const FONT_SIZE: f32 = 14.0;
const GIF_WIDTH: u32 = 800;

pub fn run() -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "ptytrace: stdin is not a terminal — bare-mode demo needs an interactive tty.\n\
             Pipe-driven users want `ptytrace capture --out X` (writes a trace) or\n\
             `ptyrender <trace> <out>` (renders an existing trace)."
        );
    }

    let trace_path = PathBuf::from(format!("{BASENAME}.trace"));
    let gif_path = PathBuf::from(format!("{BASENAME}.gif"));
    let attestation_path = PathBuf::from(format!("{BASENAME}.attestation.json"));
    let witness_path = PathBuf::from(format!("{BASENAME}.witness.json"));

    eprintln!();
    eprintln!("─── ptytrace ─────────────────────────────────────────");
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

    let attestation_sha256 = write_demo_attestation(&trace_path, &attestation_path)?;

    // 2. Render to GIF + emit witness.
    let witness = ptytrace::render(&trace_path)?
        .font_size(FONT_SIZE)
        .width(GIF_WIDTH)
        .attestation_sha256(attestation_sha256.clone())
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
    eprintln!("✓ attested   {}", attestation_path.display());
    eprintln!("✓ witness    {}", witness_path.display());
    print_hash_summary(&witness, &attestation_sha256);

    // 3. Re-verify the witness we just produced. This re-renders the
    //    trace and confirms the output bytes hash to exactly what the
    //    witness claims. The "✓ MATCH" line is the holy-shit moment —
    //    the audience sees the cryptographic round-trip in the same
    //    output as the live capture they just watched.
    let outcome = witness.verify_with_attestation(&trace_path, &attestation_path)?;
    match &outcome {
        VerifyOutcome::Match => {
            eprintln!("✓ verified   MATCH  (re-rendered, output bytes identical)");
        }
        other => {
            eprintln!("✗ verify     {other}");
        }
    }

    eprintln!();
    eprintln!("─── reproduce on any machine ──────────────────────");
    eprintln!(
        "    ptytrace verify --witness {} --trace {}",
        witness_path.display(),
        trace_path.display(),
    );
    eprintln!(
        "                  --attestation {}",
        attestation_path.display()
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

fn write_demo_attestation(trace_path: &Path, attestation_path: &Path) -> anyhow::Result<String> {
    let (trace_sha256, trace_size_bytes) = super::attestation_io::trace_sha256(trace_path)?;
    let attestation = super::attestation_io::file_attestation(
        trace_path,
        &trace_sha256,
        trace_size_bytes,
        None,
        None,
        None,
    )?;
    super::attestation_io::write_attestation(attestation_path, &attestation)
}

fn print_hash_summary(witness: &ptytrace::witness::Witness, attestation_sha256: &str) {
    eprintln!(
        "    trace_sha256:    {}…",
        short_hash(&witness.trace_sha256)
    );
    eprintln!(
        "    output_sha256:   {}…",
        short_hash(&witness.output_sha256)
    );
    if let Some(rec_sha) = &witness.tool.recorder_sha256 {
        eprintln!("    ptytrace_sha256:   {}…", short_hash(rec_sha));
    }
    if let Some(ff_sha) = &witness.tool.ffmpeg_sha256 {
        eprintln!("    ffmpeg_sha256:   {}…", short_hash(ff_sha));
    }
    eprintln!(
        "    attestation_sha256: {}…",
        short_hash(attestation_sha256)
    );
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
