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

/// Prepare a known-clean terminal substrate for post-session
/// output. The pattern (and why it has to be this aggressive):
///
/// After a PTY-captured session, the terminal can be in ANY state —
/// alt-screen on/off, cursor anywhere, scrolled, resized mid-session,
/// rows still holding cargo compile-progress lines, shell prompts, or
/// `[recording → ...]` banner residue. Per-line `\x1b[2K\r` defends
/// against the current row but not against wrapped multi-row text or
/// content on rows below. The robust answer: emit enough newlines to
/// guarantee EVERY row on the visible screen scrolled into scrollback,
/// then home the cursor on the now-blank viewport.
///
/// Why `2 × rows` newlines: when cursor starts at row K, the first
/// `(rows - 1 - K)` newlines just advance without scrolling. Only
/// newlines beyond that scroll the viewport. `2 × rows` guarantees at
/// least `rows + 1` actual scrolls regardless of starting cursor row,
/// which is enough to push any starting screen content into scrollback.
///
/// Scrollback is preserved — the user can scroll up to review the
/// captured session. The current viewport is freshly blank, cursor at
/// (0, 0). Subsequent printlns land deterministically.
///
/// No-op when stdout is piped/redirected so escape bytes + newline
/// padding don't pollute scripted consumers.
///
/// Regression covered by `crates/ptyrecord/tests/output_cleanliness.rs`.
fn prepare_clean_substrate_if_tty() {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let (_cols, rows) = detect_tty_size().unwrap_or((80, 24));
    let padding = (rows as usize).saturating_mul(2);
    print!("{}\x1b[H", "\n".repeat(padding));
}

fn detect_tty_size() -> Option<(u16, u16)> {
    use nix::libc;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `ws` is a valid &mut winsize; STDOUT_FILENO is a valid
    // fd in any hosted process (closed-stdio is UB territory).
    let ret = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut ws) };
    if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

/// Print one persistent-artifact "wrote X" line.
fn print_wrote(path: impl std::fmt::Display) {
    println!("wrote {path}");
}

use clap::Parser;
use ptyrecord::{LiveFrameStitcher, LiveStitchConfig, PtyRecord};
use ptyrender::encode::{EncodeRequest, Mp4Encoder, encode};
use ptyrender::witness::{RenderOptions, Witness};
use ptytrace::pty::{CaptureOpts, capture_with_sink};
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
    /// Suppress the default `<stem>.mp4` sidecar; write only the
    /// `.ptyrecord` bundle. Intended for CI / archival workflows where
    /// the bundle is the canonical artifact and a loose mp4 next to it
    /// is litter.
    #[arg(long, conflicts_with_all = ["trace_in", "media_out"])]
    bundle_only: bool,
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

#[allow(clippy::too_many_lines)]
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
        // No PTY session ran in this mode, so terminal state should
        // be clean already, but tools may pipe stderr around — keep
        // the message structure parallel to the live-capture path.
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
    // Trace stays embedded-only by default — humans don't consume
    // `.ptytrace` directly; tooling that wants it standalone passes
    // `--trace-out`.
    let trace_path = args
        .trace_out
        .clone()
        .unwrap_or_else(|| work.path().join(format!("{stem}.ptytrace")));
    let trace_is_sidecar = args.trace_out.is_some();
    // Media defaults to `<out_stem>.mp4` next to the bundle so the
    // user gets a file their video player can open without an extract
    // step. `--bundle-only` opts out by routing the mp4 into the
    // tempdir (where Drop deletes it after embedding into the bundle).
    let media_path = match (&args.media_out, args.bundle_only) {
        (Some(p), _) => p.clone(),
        (None, false) => out.with_extension("mp4"),
        (None, true) => work.path().join(format!("{stem}.mp4")),
    };
    let media_is_sidecar = args.media_out.is_some() || !args.bundle_only;
    ensure_mp4_path(&media_path)?;
    ensure_parent(&trace_path)?;
    ensure_parent(&media_path)?;

    let frames_dir = work.path().join(format!("{stem}-frames"));
    let mut stitcher = LiveFrameStitcher::new(
        &frames_dir,
        LiveStitchConfig {
            font_size_px: args.font_size,
            padding_px: args.padding,
        },
    );

    eprintln!(
        "[recording → {}; live-stitching frames → {}]",
        trace_path.display(),
        frames_dir.display()
    );
    let trace = capture_with_sink(
        CaptureOpts {
            argv: args.command,
            cols: None,
            rows: None,
            max_runtime: Duration::from_secs(args.max_secs),
        },
        &mut stitcher,
    )?;
    // Write the trace to disk so the encoder + bundler can read it back.
    // No announcement: when no `--trace-out` was passed, the path is
    // inside the `TempDir` and won't survive past this `main` — telling
    // the user "wrote /tmp/.tmpXXX/...ptytrace" is misleading. The
    // post-bundle print at the bottom enumerates only persistent files.
    trace.write(&trace_path)?;

    let prepared_frames = stitcher.finish()?;
    if prepared_frames.timing.is_empty() {
        anyhow::bail!("ptyrecord captured no terminal output; cannot encode media");
    }
    encode(&EncodeRequest {
        frames_dir: prepared_frames.frames_dir,
        timing: prepared_frames.timing,
        out_path: media_path.clone(),
        fps: args.fps,
        mp4_encoder: Mp4Encoder::Libx264,
        width: args.width,
    })?;

    let witness = (!args.no_witness)
        .then(|| {
            Witness::from_rendered_output(
                &trace_path,
                &media_path,
                RenderOptions::libx264(args.font_size, args.padding, args.width, args.fps),
            )
        })
        .transpose()?;
    if let (Some(witness), Some(witness_out)) = (&witness, &args.witness_out) {
        ensure_parent(witness_out)?;
        witness.write(witness_out)?;
    }

    let record = PtyRecord::from_paths(&trace_path, &media_path, witness.as_ref())?;
    record.write(&out)?;

    prepare_clean_substrate_if_tty();
    print_wrote(out.display());
    if media_is_sidecar {
        print_wrote(media_path.display());
    }
    if trace_is_sidecar {
        print_wrote(trace_path.display());
    }
    if let Some(witness_out) = &args.witness_out {
        print_wrote(witness_out.display());
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
    fn bundle_only_conflicts_with_media_out() {
        // The combination is contradictory: --bundle-only asks for the
        // mp4 to be ephemeral, --media-out names a persistent path for
        // it. Clap rejects at parse time so the runtime never sees an
        // impossible state.
        assert!(
            Args::try_parse_from([
                "ptyrecord",
                "--bundle-only",
                "--media-out",
                "demo.mp4",
                "zsh",
            ])
            .is_err()
        );
    }

    #[test]
    fn bundle_only_conflicts_with_trace_in() {
        // --trace-in mode never produces a media sidecar (the early
        // return short-circuits all default-sidecar logic), so
        // --bundle-only would be a no-op and is rejected at parse.
        assert!(
            Args::try_parse_from([
                "ptyrecord",
                "--bundle-only",
                "--trace-in",
                "demo.ptytrace",
                "--media-in",
                "demo.mp4",
            ])
            .is_err()
        );
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
