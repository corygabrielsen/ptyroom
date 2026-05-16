//! `verify` subcommand: re-render a trace and check it matches a witness,
//! optionally also re-checking a behavioral contract and provenance
//! attestation the witness anchors.

use std::path::PathBuf;

use ptyrender::witness::{VerifyOutcome, Witness};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the witness JSON.
    #[arg(long)]
    witness: PathBuf,
    /// Path to the input trace file the witness claims to describe.
    #[arg(long)]
    trace: PathBuf,
    /// Optional contract file. Required when the witness carries a
    /// `contract_sha256` claim: the contract hash must match and every
    /// predicate must pass.
    #[arg(long)]
    contract: Option<PathBuf>,
    /// Optional attestation sidecar. Required when the witness carries
    /// an `attestation_sha256` claim: the attestation hash must match
    /// and its target hash must equal the trace hash.
    #[arg(long)]
    attestation: Option<PathBuf>,
}

/// Returns true when the witness's claims are all confirmed.
pub fn run(args: &Args) -> anyhow::Result<bool> {
    let witness = Witness::read(&args.witness)?;
    let outcome = witness.verify(
        &args.trace,
        args.contract.as_deref(),
        args.attestation.as_deref(),
    )?;
    println!("{outcome}");
    Ok(matches!(outcome, VerifyOutcome::Match))
}
