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
    let outcome =
        match (&args.contract, &args.attestation) {
            (Some(contract_path), Some(attestation_path)) => witness
                .verify_with_spec_and_attestation(&args.trace, contract_path, attestation_path)?,
            (Some(contract_path), None) => witness.verify_with_spec(&args.trace, contract_path)?,
            (None, Some(attestation_path)) => {
                witness.verify_with_attestation(&args.trace, attestation_path)?
            }
            (None, None) => witness.verify(&args.trace)?,
        };
    println!("{outcome}");
    Ok(matches!(outcome, VerifyOutcome::Match))
}
