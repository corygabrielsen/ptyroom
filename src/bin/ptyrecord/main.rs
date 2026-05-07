//! `ptyrecord` CLI: compose live PTY capture with media rendering.
//!
//! Algebraically this is `bundle(ptytrace(command), ptyrender(...))`:
//! it records a command under a PTY, renders that trace to MP4, then writes one
//! `.ptyrecord` bundle containing the trace, media, selectable text, and
//! a reproducibility witness by default. It can also bundle an existing trace,
//! MP4, and witness.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use ptytrace::pty::{CaptureOpts, capture};
use ptytrace::ptyrecord::PtyRecord;
use ptytrace::witness::Witness;
use tempfile::TempDir;

#[derive(Parser)]
#[command(
    version,
    about = "ptyrecord — capture a command under a PTY and write a .ptyrecord bundle",
    long_about = "Run `ptyrecord htop` or `ptyrecord ssh host` to capture a live PTY\n\
                  session, render media, and write one `.ptyrecord` bundle containing\n\
                  the `.ptytrace`, media, selectable text, and reproducibility witness.\n\
                  Use `--trace-in T --media-in M --witness-in W` to bundle existing files."
)]
struct Args {
    /// Output ptyrecord bundle path.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Existing trace to bundle instead of recording a live command.
    #[arg(
        long,
        value_name = "TRACE",
        requires = "media_in",
        conflicts_with = "command"
    )]
    trace_in: Option<PathBuf>,
    /// Existing MP4 media to bundle with `--trace-in`.
    #[arg(
        long,
        value_name = "MEDIA",
        requires = "trace_in",
        conflicts_with = "command"
    )]
    media_in: Option<PathBuf>,
    /// Existing render witness JSON to embed with `--trace-in`.
    #[arg(
        long,
        value_name = "WITNESS",
        requires = "trace_in",
        conflicts_with = "no_witness"
    )]
    witness_in: Option<PathBuf>,
    /// Optional sidecar copy of the raw trace.
    #[arg(long, conflicts_with = "trace_in")]
    trace_out: Option<PathBuf>,
    /// Optional sidecar copy of the rendered MP4 media.
    #[arg(long, conflicts_with = "trace_in")]
    media_out: Option<PathBuf>,
    /// Optional sidecar copy of the witness JSON embedded in the bundle.
    #[arg(long, conflicts_with_all = ["no_witness", "trace_in"])]
    witness_out: Option<PathBuf>,
    /// Do not embed a reproducibility witness.
    #[arg(long)]
    no_witness: bool,
    /// Font size in pixels.
    #[arg(long, default_value_t = 14.0)]
    font_size: f32,
    /// Padding around the grid in pixels.
    #[arg(long, default_value_t = 12)]
    padding: u32,
    /// Optional output width in pixels (lanczos scaling).
    #[arg(long)]
    width: Option<u32>,
    /// Output frame rate.
    #[arg(long, default_value_t = 25)]
    fps: u32,
    /// Maximum recording duration in seconds.
    #[arg(long, default_value_t = 3600)]
    max_secs: u64,
    /// Command to run under a PTY.
    #[arg(
        value_name = "COMMAND",
        required_unless_present = "trace_in",
        num_args = 1..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let out = args.out.unwrap_or_else(default_record_path);
    ensure_parent(&out)?;

    if let Some(trace_path) = &args.trace_in {
        let media_path = args
            .media_in
            .as_ref()
            .expect("clap requires --media-in with --trace-in");
        ensure_mp4_path(media_path)?;
        let witness = args.witness_in.as_ref().map(Witness::read).transpose()?;
        let record = PtyRecord::from_paths(trace_path, media_path, witness.as_ref())?;
        record.write(&out)?;
        println!(
            "wrote {} + embedded trace {} + media {}",
            out.display(),
            record.trace.path,
            record.media.path
        );
        return Ok(());
    }

    if !std::io::stdin().is_terminal() {
        anyhow::bail!("ptyrecord: stdin is not a terminal — recording needs an interactive tty");
    }

    let work = TempDir::new()?;
    let stem = bundle_stem(&out);
    let trace_path = args
        .trace_out
        .clone()
        .unwrap_or_else(|| work.path().join(format!("{stem}.ptytrace")));
    let media_path = args
        .media_out
        .clone()
        .unwrap_or_else(|| work.path().join(format!("{stem}.mp4")));
    ensure_mp4_path(&media_path)?;
    ensure_parent(&trace_path)?;
    ensure_parent(&media_path)?;

    eprintln!("[recording → {}]", trace_path.display());
    let trace = capture(CaptureOpts {
        argv: args.command,
        cols: None,
        rows: None,
        max_runtime: Duration::from_secs(args.max_secs),
    })?;
    trace.write_with_summary(&trace_path)?;

    let mut render = ptytrace::render(&trace_path)?
        .font_size(args.font_size)
        .padding(args.padding)
        .fps(args.fps);
    if let Some(width) = args.width {
        render = render.width(width);
    }

    let witness = if args.no_witness {
        render.to_path(&media_path)?;
        None
    } else {
        let witness = render.to_path_with_receipt(&media_path)?;
        if let Some(witness_out) = &args.witness_out {
            ensure_parent(witness_out)?;
            witness.write(witness_out)?;
        }
        Some(witness)
    };

    let record = PtyRecord::from_paths(&trace_path, &media_path, witness.as_ref())?;
    record.write(&out)?;

    println!(
        "wrote {} + embedded trace {} + media {}",
        out.display(),
        record.trace.path,
        record.media.path
    );
    if let Some(witness_out) = &args.witness_out {
        println!("wrote witness {}", witness_out.display());
    }

    Ok(())
}

fn default_record_path() -> PathBuf {
    PathBuf::from(format!("recording-{}.ptyrecord", unix_secs()))
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn bundle_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("recording")
        .to_string()
}

fn ensure_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn ensure_mp4_path(path: &Path) -> anyhow::Result<()> {
    let ext = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    if ext.as_deref() != Some("mp4") {
        anyhow::bail!(
            "ptyrecord embeds browser-controllable MP4 media; got {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::{CommandFactory, Parser};

    use super::{bundle_stem, default_record_path, ensure_mp4_path};
    use crate::Args;

    #[test]
    fn default_record_path_uses_ptyrecord_extension() {
        assert_eq!(default_record_path().extension().unwrap(), "ptyrecord");
    }

    #[test]
    fn bundle_stem_uses_output_file_stem() {
        assert_eq!(bundle_stem(Path::new("demo.ptyrecord")), "demo");
    }

    #[test]
    fn media_sidecar_must_be_mp4() {
        assert!(ensure_mp4_path(Path::new("demo.mp4")).is_ok());
        assert!(ensure_mp4_path(Path::new("demo.gif")).is_err());
    }

    #[test]
    fn command_mode_still_accepts_raw_argv() {
        let args = Args::try_parse_from(["ptyrecord", "ssh", "host"]).unwrap();

        assert_eq!(args.command, ["ssh", "host"]);
        assert!(args.trace_in.is_none());
    }

    #[test]
    fn bundle_mode_accepts_existing_trace_and_media() {
        let args = Args::try_parse_from([
            "ptyrecord",
            "--trace-in",
            "demo.ptytrace",
            "--media-in",
            "demo.mp4",
            "--out",
            "demo.ptyrecord",
        ])
        .unwrap();

        assert_eq!(args.trace_in.unwrap(), Path::new("demo.ptytrace"));
        assert_eq!(args.media_in.unwrap(), Path::new("demo.mp4"));
        assert!(args.command.is_empty());
    }

    #[test]
    fn bundle_mode_requires_media() {
        assert!(Args::try_parse_from(["ptyrecord", "--trace-in", "demo.ptytrace"]).is_err());
    }

    #[test]
    fn help_mentions_existing_file_bundle_mode() {
        let help = Args::command().render_long_help().to_string();

        assert!(help.contains("--trace-in"));
        assert!(help.contains("--media-in"));
        assert!(help.contains("--witness-in"));
    }
}
