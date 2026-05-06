//! `check` subcommand: replay a trace and verify it against a [`Contract`].

use std::path::PathBuf;

use tracer::contract::Contract;
use tracer::trace::Trace;

#[derive(clap::Args)]
pub struct Args {
    /// Path to the trace file to replay.
    #[arg(long)]
    trace: PathBuf,
    /// Path to the contract JSON containing predicates to evaluate.
    #[arg(long)]
    contract: PathBuf,
}

/// Returns true when every predicate in the contract passes.
pub fn run(args: &Args) -> anyhow::Result<bool> {
    let trace = Trace::read(&args.trace)?;
    let contract = Contract::read(&args.contract)?;
    let report = contract.check(&trace);
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
