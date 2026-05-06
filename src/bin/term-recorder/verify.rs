//! `verify` subcommand: re-render a cast and check it matches a receipt.

use std::path::PathBuf;

use term_recorder::receipt::{Receipt, VerifyOutcome};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the receipt JSON.
    #[arg(long)]
    receipt: PathBuf,
    /// Path to the input cast file the receipt claims to describe.
    #[arg(long)]
    cast: PathBuf,
}

/// Returns true when the receipt's claims are all confirmed.
pub fn run(args: &Args) -> anyhow::Result<bool> {
    let receipt = Receipt::read(&args.receipt)?;
    let outcome = receipt.verify(&args.cast)?;
    println!("{outcome}");
    Ok(matches!(outcome, VerifyOutcome::Match))
}
