//! `capture` subcommand: live, interactive terminal session recording.
//!
//! Spawns a child shell under a PTY, puts the host's stdin in raw
//! mode, tees PTY output to the host's stdout, and writes a trace.
//! Mirrors the asciinema recording UX: just press the key and start typing.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ptytrace::pty::{CaptureOpts, capture};

#[derive(clap::Args)]
pub struct Args {
    /// Output trace path. Default: `recording-<unix-secs>.ptytrace` in the
    /// current directory.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Maximum recording duration in seconds before the recorder
    /// force-stops. Default: 3600 (1 hour).
    #[arg(long, default_value_t = 3600)]
    max_secs: u64,
    /// Shell argv (default: `$SHELL`, falling back to `bash`).
    /// Pass after `--`, e.g. `ptytrace capture -- /usr/bin/zsh -i`.
    #[arg(last = true)]
    argv: Vec<String>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let out = args.out.unwrap_or_else(default_out_path);
    capture_to_path(&out, args.argv, args.max_secs)
}

pub fn run_command(argv: Vec<String>) -> anyhow::Result<()> {
    let out = default_out_path();
    capture_to_path(&out, argv, 3600)
}

pub fn capture_to_path(out: &Path, argv: Vec<String>, max_secs: u64) -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("capture: stdin is not a terminal — live recording needs an interactive tty");
    }

    eprintln!(
        "[recording → {}]  type 'exit' or Ctrl-D to stop",
        out.display()
    );

    let trace = capture(CaptureOpts {
        argv,
        cols: None,
        rows: None,
        max_runtime: Duration::from_secs(max_secs),
    })?;

    trace.write(out)?;
    // Per-row clear before the println. See `ptyrecord/INVARIANTS.md`
    // for the contract — `INVARIANT_USER_SCROLLBACK_PRESERVED` and
    // `INVARIANT_USER_TERMINAL_NOT_CLEARED` forbid screen-wide
    // manipulation; per-row clear is permitted because it only touches
    // the row we're about to overwrite.
    // `INVARIANT_PIPED_STDOUT_IS_PLAIN` gates the escape on tty.
    if std::io::stdout().is_terminal() {
        print!("\x1b[2K\r");
    }
    println!(
        "wrote {} ({} bytes, {} events)",
        out.display(),
        std::fs::metadata(out)?.len(),
        trace.events.len()
    );
    Ok(())
}

pub fn default_out_path() -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    PathBuf::from(format!("recording-{secs}.ptytrace"))
}
