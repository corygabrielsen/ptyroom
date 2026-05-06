//! `check` subcommand: replay a cast and verify it against a [`Contract`].

use std::path::PathBuf;

use tracer::contract::Contract;
use tracer::trace::Trace;

#[derive(clap::Args)]
pub struct Args {
    /// Path to the cast file to replay.
    #[arg(long)]
    cast: PathBuf,
    /// Path to the spec JSON containing predicates to evaluate.
    #[arg(long)]
    spec: PathBuf,
}

/// Returns true when every predicate in the spec passes.
pub fn run(args: &Args) -> anyhow::Result<bool> {
    let cast = Trace::read(&args.cast)?;
    let spec = Contract::read(&args.spec)?;
    let report = spec.check(&cast);
    for outcome in &report.outcomes {
        println!("{outcome}");
    }
    if report.all_passed() {
        println!("ALL_PASS {} predicate(s)", report.outcomes.len());
    } else {
        println!(
            "FAIL {}/{} predicate(s)",
            report.failed_count(),
            report.outcomes.len()
        );
    }
    Ok(report.all_passed())
}
