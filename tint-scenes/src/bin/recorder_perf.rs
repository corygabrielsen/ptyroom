//! Isolated recorder-leg benchmark.
//!
//! This intentionally measures capture wall time only. It does not snapshot,
//! paint, encode, or write casts; use it to see whether startup, typing,
//! prompt round trips, `tint` commands, or picker interaction is currently
//! dominating recorder latency.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use term_recorder::recorder::{Recorder, RecorderConfig};
use tint_scenes::scenes::{
    TYPE_COMMAND, TYPE_LABEL, blank, lookup_picker_idx, ms, run_cli, run_picker, wait_for_prompt,
};

const PICKER_TARGET: &str = "dark-azure";

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Case {
    Startup,
    Typing,
    Prompt,
    Tint,
    Picker,
    All,
}

#[derive(Parser)]
#[command(about = "Measure isolated tint-recorder capture legs")]
struct Args {
    #[arg(long, value_enum, default_value_t = Case::All)]
    case: Case,
    #[arg(long, default_value_t = 5)]
    iterations: usize,
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("case,iteration,wall_us,events");
    for case in expand_cases(args.case) {
        for iteration in 1..=args.iterations {
            let (elapsed, events) = run_case(case, &args.tint_path)?;
            println!("{case:?},{iteration},{},{}", elapsed.as_micros(), events);
        }
    }
    Ok(())
}

fn expand_cases(case: Case) -> Vec<Case> {
    match case {
        Case::All => vec![
            Case::Startup,
            Case::Typing,
            Case::Prompt,
            Case::Tint,
            Case::Picker,
        ],
        other => vec![other],
    }
}

fn run_case(case: Case, tint_path: &std::path::Path) -> anyhow::Result<(Duration, usize)> {
    let started = Instant::now();
    let mut recorder = Recorder::start(RecorderConfig {
        rows: 20,
        ..tint_scenes::scenes::tint_recorder_config()
    })?;
    wait_for_prompt(&mut recorder, ms(0), "startup prompt")?;

    match case {
        Case::Startup => {}
        Case::Typing => {
            recorder.type_text("# tint -- batched typing benchmark", TYPE_LABEL)?;
            recorder.type_text("tint solarized-light", TYPE_COMMAND)?;
            recorder.type_text("mkdir foo && echo pale-sky-blue > foo/.tint", TYPE_COMMAND)?;
        }
        Case::Prompt => {
            for _ in 0..20 {
                blank(&mut recorder, ms(0))?;
            }
        }
        Case::Tint => run_cli(&mut recorder)?,
        Case::Picker => {
            let down_to_target = lookup_picker_idx(tint_path, PICKER_TARGET)?;
            run_picker(&mut recorder, down_to_target)?;
        }
        Case::All => unreachable!("expanded before dispatch"),
    }

    let events = recorder.event_count();
    recorder.stop()?;
    Ok((started.elapsed(), events))
}
