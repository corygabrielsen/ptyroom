//! Pipeline test orchestration.
//!
//! Drives the full record → snapshot → paint → encode pipeline for one
//! scene and produces a fixed-shape hash dictionary describing every
//! artifact layer. Used by the `pipeline-test` binary to power
//! `characterize`, `bless`, `verify`, and `render` subcommands.
//!
//! No tint coupling lives here — the only assumption is that a scene
//! name maps to an existing `target/release/<scene>` binary that
//! accepts `--cast <path>` and writes an asciicast. Hashing reads
//! files; nothing here knows or cares about themes or pickers.
//!
//! Working directory must be the project root (where `Makefile`,
//! `assets/` and `target/` live).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Knobs shared by every pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineOptions {
    /// Docker container name to use as the warm recorder.
    pub warm_container: String,
    /// Painter font size in pixels (cell pitch).
    pub font_size: u32,
    /// GIF output width (lanczos scale target).
    pub gif_width: u32,
    /// Path to the host `tint` script (passed to scene binaries via
    /// the `TINT_PATH` env var).
    pub tint_path: PathBuf,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            warm_container: std::env::var("WARM_CONTAINER")
                .unwrap_or_else(|_| "term-recorder-warm".into()),
            font_size: 40,
            gif_width: 824,
            tint_path: std::env::var("TINT_PATH")
                .map_or_else(|_| PathBuf::from("../tint/tint"), PathBuf::from),
        }
    }
}

/// Hash dictionary covering every artifact layer the goldens pin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PipelineHashes {
    pub concat_o: String,
    pub cast_event_count: usize,
    pub final_snapshot: String,
    pub all_snapshots: String,
    pub snapshot_count: usize,
    pub all_pngs: String,
    pub png_count: usize,
    pub mp4: String,
    pub gif: String,
}

/// Committed golden file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Golden {
    pub scene: String,
    pub blessed_at: String,
    pub blessed_runs: usize,
    pub hashes: PipelineHashes,
}

const VALID_SCENES: &[&str] = &["cli", "picker", "cd_hook", "custom_theme", "demo_full"];

/// Verify that `scene` is one of the known pipeline-test scenes.
///
/// # Errors
/// Unknown scene name.
pub fn validate_scene(scene: &str) -> anyhow::Result<()> {
    if VALID_SCENES.contains(&scene) {
        Ok(())
    } else {
        anyhow::bail!(
            "unknown scene: {scene} (valid: {})",
            VALID_SCENES.join(", ")
        )
    }
}

fn cast_path(scene: &str) -> PathBuf {
    PathBuf::from(format!("assets/{scene}.cast"))
}
fn snaps_dir(scene: &str) -> PathBuf {
    PathBuf::from(format!("assets/{scene}_snapshots"))
}
fn frames_dir(scene: &str) -> PathBuf {
    PathBuf::from(format!("assets/{scene}_frames"))
}
fn mp4_path(scene: &str) -> PathBuf {
    PathBuf::from(format!("assets/{scene}.mp4"))
}
fn gif_path(scene: &str) -> PathBuf {
    PathBuf::from(format!("assets/{scene}.gif"))
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("command failed: {cmd:?} (status={status})");
    }
    Ok(())
}

fn run_quiet(cmd: &mut Command) -> anyhow::Result<()> {
    use std::process::Stdio;
    let out = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "command failed: {cmd:?}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Run the full record → snapshot → paint → encode pipeline for one
/// scene against a warm Docker container. Verification is the caller's
/// job (the `verify` binary or a contract check).
///
/// # Errors
/// Any subprocess failure, filesystem error, or unknown scene name.
pub fn run_pipeline(scene: &str, opts: &PipelineOptions) -> anyhow::Result<()> {
    validate_scene(scene)?;
    let cast = cast_path(scene);
    let snaps = snaps_dir(scene);
    let frames = frames_dir(scene);
    let mp4 = mp4_path(scene);
    let gif = gif_path(scene);

    // Idempotence: clear stage outputs that the pipeline rebuilds.
    let _ = fs::remove_dir_all(&snaps);
    let _ = fs::remove_dir_all(&frames);

    // 1. Record. Scene binary reads TINT_PATH and TERM_RECORDER_CONTAINER
    //    from env; output is the asciicast at `cast`.
    run_quiet(
        Command::new(format!("./target/release/{scene}"))
            .arg("--cast")
            .arg(&cast)
            .env("TINT_PATH", &opts.tint_path)
            .env("TERM_RECORDER_CONTAINER", &opts.warm_container),
    )?;

    // 2. Snapshot. vt100 + OscTracker consume the cast and emit
    //    per-frame JSON + timing.json.
    run_quiet(
        Command::new("./target/release/term-recorder")
            .arg("snapshot")
            .arg(&cast)
            .arg(&snaps),
    )?;

    // 3. Paint. Each snapshot JSON → PNG. rayon-parallel internally.
    run_quiet(
        Command::new("./target/release/term-recorder")
            .arg("paint")
            .arg("--font-size")
            .arg(opts.font_size.to_string())
            .arg(&snaps)
            .arg(&frames),
    )?;

    // 4. Encode mp4 + gif from the same frame set; ffmpeg invocations
    //    run concurrently. Each write to a per-call concat tempfile so
    //    they can't race on disk.
    let timing_json = snaps.join("timing.json");
    let mut mp4_cmd = Command::new("./target/release/term-recorder");
    mp4_cmd
        .arg("encode")
        .arg(&frames)
        .arg(&timing_json)
        .arg(&mp4);
    let mp4_child = mp4_cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let mut gif_cmd = Command::new("./target/release/term-recorder");
    gif_cmd
        .arg("encode")
        .arg(&frames)
        .arg(&timing_json)
        .arg(&gif)
        .arg("--width")
        .arg(opts.gif_width.to_string());
    let gif_child = gif_cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let mp4_status = mp4_child.wait_with_output()?;
    let gif_status = gif_child.wait_with_output()?;
    if !mp4_status.status.success() {
        anyhow::bail!("mp4 encode failed: {:?}", mp4_status.status);
    }
    if !gif_status.status.success() {
        anyhow::bail!("gif encode failed: {:?}", gif_status.status);
    }

    Ok(())
}

/// Run the per-scene verify contract against the snapshot dir.
///
/// # Errors
/// Subprocess failure or non-zero exit from `target/release/verify`.
pub fn run_verify(scene: &str) -> anyhow::Result<()> {
    validate_scene(scene)?;
    run(Command::new("./target/release/verify")
        .arg(scene)
        .arg("--snapshots-dir")
        .arg(snaps_dir(scene)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 64 KiB stream buffer for sha256 file hashing. Heap-allocated so
/// clippy's stack-size lint doesn't complain about a large fixed-size
/// stack array; the allocation happens once per call and is amortized
/// across many `read` syscalls.
const HASH_BUF_LEN: usize = 64 * 1024;

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut h = Sha256::new();
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; HASH_BUF_LEN];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn sha256_files_concat(paths: &[PathBuf]) -> anyhow::Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; HASH_BUF_LEN];
    for p in paths {
        let mut f = fs::File::open(p)?;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
    }
    Ok(hex(&h.finalize()))
}

fn sorted_numbered<P: AsRef<Path>>(dir: P, ext: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some(ext)
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    entries.sort();
    Ok(entries)
}

/// Hash every artifact layer for `scene` after a `run_pipeline` call.
///
/// # Errors
/// Filesystem or parse error reading any artifact.
///
/// # Panics
/// Panics if the snapshots directory exists but is empty after the
/// non-empty check passes (impossible by construction; the `expect`
/// is a defense-in-depth assertion).
pub fn hash_pipeline(scene: &str) -> anyhow::Result<PipelineHashes> {
    validate_scene(scene)?;
    let cast = cast_path(scene);
    let snaps = snaps_dir(scene);
    let frames = frames_dir(scene);

    let cast_text = fs::read_to_string(&cast)?;
    let mut events_iter = cast_text.lines().filter(|l| !l.is_empty());
    let _header = events_iter.next();
    let mut event_count: usize = 0;
    let mut concat_h = Sha256::new();
    for line in events_iter {
        event_count += 1;
        let ev: serde_json::Value = serde_json::from_str(line)?;
        let arr = ev
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("cast event not array: {line}"))?;
        if arr.get(1).and_then(|v| v.as_str()) == Some("o")
            && let Some(data) = arr.get(2).and_then(|v| v.as_str())
        {
            concat_h.update(data.as_bytes());
        }
    }

    let snap_files = sorted_numbered(&snaps, "json")?;
    if snap_files.is_empty() {
        anyhow::bail!("no snapshot files in {}", snaps.display());
    }
    let final_snapshot = sha256_file(snap_files.last().expect("non-empty"))?;
    let all_snapshots = sha256_files_concat(&snap_files)?;
    let snapshot_count = snap_files.len();

    let png_files = sorted_numbered(&frames, "png")?;
    if png_files.is_empty() {
        anyhow::bail!("no png files in {}", frames.display());
    }
    let all_pngs = sha256_files_concat(&png_files)?;
    let png_count = png_files.len();

    let mp4 = sha256_file(&mp4_path(scene))?;
    let gif = sha256_file(&gif_path(scene))?;

    Ok(PipelineHashes {
        concat_o: hex(&concat_h.finalize()),
        cast_event_count: event_count,
        final_snapshot,
        all_snapshots,
        snapshot_count,
        all_pngs,
        png_count,
        mp4,
        gif,
    })
}

/// Default scene set used when no `SCENES` argument is supplied.
#[must_use]
pub fn default_scenes() -> Vec<String> {
    VALID_SCENES.iter().map(|s| (*s).to_string()).collect()
}

/// Exhaustively-checked field accessors for diff output.
impl PipelineHashes {
    /// Iterate fields in the canonical layer order used by the report.
    pub fn iter_layers(&self) -> impl Iterator<Item = (&'static str, String)> + '_ {
        [
            ("concat_o", self.concat_o.clone()),
            ("cast_event_count", self.cast_event_count.to_string()),
            ("final_snapshot", self.final_snapshot.clone()),
            ("all_snapshots", self.all_snapshots.clone()),
            ("snapshot_count", self.snapshot_count.to_string()),
            ("all_pngs", self.all_pngs.clone()),
            ("png_count", self.png_count.to_string()),
            ("mp4", self.mp4.clone()),
            ("gif", self.gif.clone()),
        ]
        .into_iter()
    }
}

/// Suppress `unused` lint on the helper that's currently only used in
/// tests.
#[allow(dead_code)]
fn _phantom_use_sha256_hex() {
    let _ = sha256_hex(b"");
}
