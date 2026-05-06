//! Pipeline test driver. Replaces the bash scripts that previously
//! orchestrated the full pipeline. Subcommands:
//!
//! - `characterize`: run each scene N times, hash every layer, write
//!   `target/characterize/{<scene>.jsonl, report.md}`. Used to verify
//!   determinism before blessing goldens.
//! - `bless`: run each scene N=10 (default), refuse to write a golden
//!   if any layer disagrees across runs, otherwise emit
//!   `goldens/<scene>.json`.
//! - `verify`: run each scene once, diff each layer hash against the
//!   committed golden, print PASS/FAIL per layer. Exit non-zero on
//!   any FAIL or missing golden.
//! - `render`: render one scene (record → snapshot → paint → encode →
//!   verify). Used by `make demo-features` as the per-scene worker.
//!
//! Run from the project root.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use tint_scenes::pipeline_test::{
    Golden, PipelineHashes, PipelineOptions, default_scenes, hash_pipeline, run_pipeline,
    run_verify, validate_scene,
};

#[derive(Parser)]
#[command(name = "pipeline-test")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run each scene N times, report which layers are stable vs
    /// drift across runs.
    Characterize {
        /// Scenes to drive (default: all).
        #[arg(long, value_delimiter = ',')]
        scenes: Vec<String>,
        /// Iterations per scene.
        #[arg(long, default_value_t = 3)]
        runs: usize,
    },
    /// Bake goldens. Refuses to write if any layer disagrees across
    /// the configured number of runs.
    Bless {
        /// Scenes to bless (default: all).
        #[arg(long, value_delimiter = ',')]
        scenes: Vec<String>,
        /// Agreement gate. Must be ≥ 2; ≥ 10 recommended for the
        /// gate to actually catch realistic non-determinism.
        #[arg(long, default_value_t = 10)]
        runs: usize,
        /// Output directory for goldens.
        #[arg(long, default_value = "goldens")]
        golden_dir: PathBuf,
    },
    /// Verify current pipeline output matches committed goldens.
    Verify {
        /// Scenes to verify (default: all).
        #[arg(long, value_delimiter = ',')]
        scenes: Vec<String>,
        /// Input directory for goldens.
        #[arg(long, default_value = "goldens")]
        golden_dir: PathBuf,
    },
    /// Render one scene end-to-end (record + snapshot + paint + encode
    /// + verify). Used by `make demo-features` workers.
    Render {
        /// Scene name.
        scene: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let opts = PipelineOptions::default();
    let result = match cli.cmd {
        Cmd::Characterize { scenes, runs } => {
            characterize(scenes_or_default(scenes), runs, &opts)
        }
        Cmd::Bless {
            scenes,
            runs,
            golden_dir,
        } => bless(scenes_or_default(scenes), runs, &golden_dir, &opts),
        Cmd::Verify {
            scenes,
            golden_dir,
        } => verify(scenes_or_default(scenes), &golden_dir, &opts),
        Cmd::Render { scene } => render_one(&scene, &opts),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e:?}");
            ExitCode::from(2)
        }
    }
}

fn scenes_or_default(scenes: Vec<String>) -> Vec<String> {
    if scenes.is_empty() {
        default_scenes()
    } else {
        scenes
    }
}

fn render_one(scene: &str, opts: &PipelineOptions) -> anyhow::Result<bool> {
    validate_scene(scene)?;
    println!("=== render {scene} ===");
    run_pipeline(scene, opts)?;
    run_verify(scene)?;
    println!(
        "wrote assets/{scene}.mp4 + assets/{scene}.gif"
    );
    Ok(true)
}

fn iso_now_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn epoch_to_ymdhms(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    // Civil-from-days algorithm by Howard Hinnant. Pure-Rust UTC
    // formatter with no chrono/time crate dependency.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let h = (rem / 3_600) as u32;
    let mi = ((rem % 3_600) / 60) as u32;
    let s = (rem % 60) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d, h, mi, s)
}

fn characterize(
    scenes: Vec<String>,
    runs: usize,
    opts: &PipelineOptions,
) -> anyhow::Result<bool> {
    let out_dir = PathBuf::from("target/characterize");
    fs::create_dir_all(&out_dir)?;
    let report_path = out_dir.join("report.md");
    let mut report = String::new();
    report.push_str("# Determinism characterization\n\n");
    report.push_str(&format!("- Generated: {}\n", iso_now_utc()));
    report.push_str(&format!("- Scenes:    {}\n", scenes.join(" ")));
    report.push_str(&format!("- Runs:      {runs}\n\n"));

    for scene in &scenes {
        validate_scene(scene)?;
        eprintln!("=== characterizing {scene} (runs={runs}) ===");
        let jsonl_path = out_dir.join(format!("{scene}.jsonl"));
        let mut jsonl = fs::File::create(&jsonl_path)?;
        let mut runs_hashes: Vec<PipelineHashes> = Vec::with_capacity(runs);
        for i in 0..runs {
            eprintln!("  run {i}");
            run_pipeline(scene, opts)?;
            let h = hash_pipeline(scene)?;
            writeln!(jsonl, "{}", serde_json::to_string(&h)?)?;
            runs_hashes.push(h);
        }

        report.push_str(&format!("## {scene}\n\n"));
        report.push_str("| layer | status | distinct | sample |\n");
        report.push_str("|---|---|---|---|\n");
        let layers = ["concat_o", "cast_event_count", "final_snapshot", "all_snapshots",
                      "snapshot_count", "all_pngs", "png_count", "mp4", "gif"];
        for layer in layers {
            let values: Vec<String> = runs_hashes
                .iter()
                .filter_map(|h| h.iter_layers().find(|(k, _)| *k == layer).map(|(_, v)| v))
                .collect();
            let mut distinct = values.clone();
            distinct.sort();
            distinct.dedup();
            let status = if distinct.len() == 1 { "STABLE" } else { "VARIES" };
            let sample = values.first().map(String::as_str).unwrap_or("");
            let sample = if sample.len() > 16 {
                format!("{}…", &sample[..12])
            } else {
                sample.to_string()
            };
            report.push_str(&format!(
                "| {layer} | {status} | {} | {sample} |\n",
                distinct.len()
            ));
        }
        report.push('\n');
    }
    fs::write(&report_path, report)?;
    eprintln!();
    eprintln!("report: {}", report_path.display());
    eprintln!("raw:    {}/<scene>.jsonl", out_dir.display());
    Ok(true)
}

fn bless(
    scenes: Vec<String>,
    runs: usize,
    golden_dir: &PathBuf,
    opts: &PipelineOptions,
) -> anyhow::Result<bool> {
    if runs < 2 {
        anyhow::bail!("--runs must be ≥ 2 (agreement gate); got {runs}");
    }
    fs::create_dir_all(golden_dir)?;
    let mut all_ok = true;
    for scene in &scenes {
        validate_scene(scene)?;
        eprintln!("=== blessing {scene} (runs={runs}) ===");
        let mut history: Vec<PipelineHashes> = Vec::with_capacity(runs);
        for i in 0..runs {
            eprintln!("  run {i}");
            run_pipeline(scene, opts)?;
            history.push(hash_pipeline(scene)?);
        }
        let first = history.first().expect("history non-empty");
        let agree = history.iter().all(|h| h == first);
        if !agree {
            eprintln!(
                "REFUSE bless {scene}: layers disagreed across {runs} runs"
            );
            for (i, h) in history.iter().enumerate() {
                eprintln!("  run {i}: {}", serde_json::to_string(h)?);
            }
            all_ok = false;
            continue;
        }
        let golden = Golden {
            scene: scene.clone(),
            blessed_at: iso_now_utc(),
            blessed_runs: runs,
            hashes: first.clone(),
        };
        let out_path = golden_dir.join(format!("{scene}.json"));
        fs::write(&out_path, serde_json::to_string_pretty(&golden)? + "\n")?;
        eprintln!("  wrote {}", out_path.display());
    }
    Ok(all_ok)
}

fn verify(
    scenes: Vec<String>,
    golden_dir: &PathBuf,
    opts: &PipelineOptions,
) -> anyhow::Result<bool> {
    let mut all_ok = true;
    for scene in &scenes {
        validate_scene(scene)?;
        let golden_path = golden_dir.join(format!("{scene}.json"));
        let golden_text = match fs::read_to_string(&golden_path) {
            Ok(t) => t,
            Err(_) => {
                println!("FAIL  {scene}: no golden at {}", golden_path.display());
                all_ok = false;
                continue;
            }
        };
        let golden: Golden = serde_json::from_str(&golden_text)?;

        run_pipeline(scene, opts)?;
        let current = hash_pipeline(scene)?;

        for (layer, current_v) in current.iter_layers() {
            let golden_v = golden
                .hashes
                .iter_layers()
                .find(|(k, _)| *k == layer)
                .map(|(_, v)| v)
                .expect("layer in PipelineHashes");
            if golden_v == current_v {
                println!("PASS  {scene}/{layer}");
            } else {
                println!(
                    "FAIL  {scene}/{layer}\n      golden={golden_v}\n      current={current_v}"
                );
                all_ok = false;
            }
        }
    }
    Ok(all_ok)
}
