//! `rec` subcommand: live, interactive terminal session recording.
//!
//! Spawns a child shell under a PTY, puts the host's stdin in raw
//! mode, tees PTY output to the host's stdout, and writes a cast.
//! Mirrors the asciinema `rec` UX: just press the key and start typing.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracer::tracer::{CaptureOpts, capture};

#[derive(clap::Args)]
pub struct Args {
    /// Output cast path. Default: `recording-<unix-secs>.cast` in the
    /// current directory.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Maximum recording duration in seconds before the recorder
    /// force-stops. Default: 3600 (1 hour).
    #[arg(long, default_value_t = 3600)]
    max_secs: u64,
    /// Shell argv (default: `$SHELL`, falling back to `bash`).
    /// Pass after `--`, e.g. `tracer rec -- /usr/bin/zsh -i`.
    #[arg(last = true)]
    argv: Vec<String>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("rec: stdin is not a terminal — live recording needs an interactive tty");
    }

    let out = args.out.unwrap_or_else(default_out_path);

    eprintln!(
        "[recording → {}]  type 'exit' or Ctrl-D to stop",
        out.display()
    );

    let cast = capture(CaptureOpts {
        argv: args.argv,
        cols: None,
        rows: None,
        max_runtime: Duration::from_secs(args.max_secs),
    })?;

    cast.write_with_summary(&out)?;
    Ok(())
}

fn default_out_path() -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    PathBuf::from(format!("recording-{secs}.cast"))
}
