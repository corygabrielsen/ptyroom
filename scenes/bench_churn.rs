//! Color-churn benchmark scene for encode-cost measurement.
//!
//! Cycles rapidly through a wide spread of built-in themes. Each
//! `tint <theme>` invocation emits OSC sequences that flip the
//! snapshot bg + 16 ANSI palette, so the painted PNG frames have
//! many distinct color sets. Used to measure encode-cost scaling:
//!
//! - GIF palette-gen: must pick 256 colors out of a much wider pool;
//!   wider pool = more work.
//! - MP4 (libx264): inter-frame deltas are large at every theme flip,
//!   raising bitrate.
//!
//! Length and event count are tuned so the scene runs in ~12s of cast
//! time, comparable to one `demo_full` subloop. Pair with `bench_tiny`
//! to separate fixed pipeline overhead from per-frame work.
//!
//! Open contract: no verify checks. Pipeline runs end-to-end regardless
//! of which themes are reached.

use std::path::PathBuf;

use clap::Parser;
use tint_recorder::recorder::{Recorder, RecorderConfig};
use tint_recorder::scenes::{line, ms};

/// Themes spread across the curated palette so consecutive bgs are
/// visually distinct. Mix of dark/light, warm/cool, and several
/// distinct tonal families to maximize palette diversity.
const THEMES: &[&str] = &[
    "dracula",
    "solarized-light",
    "monokai",
    "nord",
    "gruvbox-dark",
    "tokyo",
    "catppuccin-mocha",
    "everforest-light",
    "horizon",
    "kanagawa",
    "rose-pine",
    "synthwave",
];

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "assets/bench_churn.cast")]
    cast: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut r = Recorder::start(RecorderConfig {
        cols: 80,
        rows: 16,
        ..Default::default()
    })?;

    // Initial bash-echo settle, invisible to the rendered output.
    r.dwell(ms(0), ms(600))?;

    // Tight cycle: type, fire, brief settle so the bg-flip lands in
    // the cast, then move on. ~1s per theme × 12 themes ≈ 12s.
    for theme in THEMES {
        line(&mut r, &format!("tint {theme}"), ms(20), ms(150), ms(400))?;
    }

    let cast = r.stop()?;
    cast.write_with_summary(&args.cast)?;
    Ok(())
}
