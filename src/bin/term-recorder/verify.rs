//! `verify` subcommand: re-render a cast and check it matches a receipt,
//! optionally also re-checking a behavioral spec the receipt attests.

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
    /// Optional spec file. Required when the receipt carries a
    /// `spec_sha256` claim — the spec hash must match and every
    /// predicate must pass.
    #[arg(long)]
    spec: Option<PathBuf>,
}

/// Returns true when the receipt's claims are all confirmed.
pub fn run(args: &Args) -> anyhow::Result<bool> {
    let receipt = Receipt::read(&args.receipt)?;
    let outcome = match &args.spec {
        Some(spec_path) => receipt.verify_with_spec(&args.cast, spec_path)?,
        None => receipt.verify(&args.cast)?,
    };
    println!("{outcome}");
    Ok(matches!(outcome, VerifyOutcome::Match))
}
