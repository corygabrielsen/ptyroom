//! `render` subcommand: trace file → MP4/GIF in one call, with optional
//! receipt, behavioral contract, and provenance attestation.

use std::path::PathBuf;

use crate::witness::sha256_hex;

#[derive(clap::Args)]
pub struct Args {
    /// Input trace file (asciinema v2 JSONL).
    trace: PathBuf,
    /// Output media path. Format inferred from extension (.mp4 or .gif).
    out: PathBuf,
    /// Font size in pixels.
    #[arg(long, default_value_t = 14.0)]
    font_size: f32,
    /// Padding around the grid in pixels.
    #[arg(long, default_value_t = 12)]
    padding: u32,
    /// Optional output width in pixels (lanczos scaling).
    #[arg(long)]
    width: Option<u32>,
    /// Output frame rate.
    #[arg(long, default_value_t = 25)]
    fps: u32,
    /// Optional receipt path. If set, a JSON receipt is written
    /// alongside the output for later reproducibility verification.
    #[arg(long)]
    receipt: Option<PathBuf>,
    /// Optional behavioral spec path. When set, the spec file's hash
    /// is embedded in the receipt so verifiers can require the
    /// matching contract via `ptyrender verify --contract`. Requires
    /// `--receipt` to be set.
    #[arg(long, requires = "receipt")]
    spec: Option<PathBuf>,
    /// Source `.script` file to hash into the receipt as provenance.
    #[arg(long, requires = "receipt")]
    script: Option<PathBuf>,
    /// Existing attestation sidecar to hash into the receipt. The
    /// attestation must target this trace's SHA-256.
    #[arg(long, requires = "receipt", conflicts_with = "attestation_out")]
    attestation: Option<PathBuf>,
    /// Write an unsigned local-file attestation for this trace and hash
    /// it into the receipt.
    #[arg(long, requires = "receipt", conflicts_with = "attestation")]
    attestation_out: Option<PathBuf>,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let (trace_sha256, trace_size_bytes) = ptytrace::attestation_io::trace_sha256(&args.trace)?;
    let mut r = crate::render(&args.trace)?
        .font_size(args.font_size)
        .padding(args.padding)
        .fps(args.fps);
    if let Some(w) = args.width {
        r = r.width(w);
    }
    if let Some(spec_path) = &args.spec {
        // Hash the canonical form of the contract, not the raw file
        // bytes. Whitespace / key-ordering differences from older
        // serde_json versions or hand-edits would otherwise produce a
        // different hash for a semantically identical contract and
        // silently break receipt verification.
        let spec = ptytrace::contract::Contract::read(spec_path)?;
        r = r.contract_sha256(sha256_hex(&spec.canonical_bytes()?));
    }
    if let Some(script_path) = &args.script {
        let script_bytes = std::fs::read(script_path)?;
        r = r.script_sha256(sha256_hex(&script_bytes));
    }
    let mut attestation_written = None;
    if let Some(attestation_path) = &args.attestation {
        let attestation_sha256 = ptytrace::attestation_io::attestation_sha256_for_trace(
            attestation_path,
            &trace_sha256,
        )?;
        r = r.attestation_sha256(attestation_sha256);
    } else if let Some(attestation_path) = &args.attestation_out {
        let attestation = ptytrace::attestation_io::file_attestation(
            &args.trace,
            &trace_sha256,
            trace_size_bytes,
            None,
            None,
            None,
        )?;
        let attestation_sha256 =
            ptytrace::attestation_io::write_attestation(attestation_path, &attestation)?;
        r = r.attestation_sha256(attestation_sha256);
        attestation_written = Some(attestation_path);
    }

    if let Some(receipt_path) = &args.receipt {
        let receipt = r.to_path_with_receipt(&args.out)?;
        receipt.write(receipt_path)?;
        let suffix = receipt_suffix(args.spec.is_some(), receipt.attestation_sha256.is_some());
        println!(
            "wrote {} + receipt {}{}",
            args.out.display(),
            receipt_path.display(),
            suffix,
        );
        if let Some(attestation_path) = attestation_written {
            println!("wrote attestation {}", attestation_path.display());
        }
    } else {
        r.to_path(&args.out)?;
        println!("wrote {}", args.out.display());
    }
    Ok(())
}

fn receipt_suffix(has_spec: bool, has_attestation: bool) -> &'static str {
    match (has_spec, has_attestation) {
        (true, true) => " (spec + attestation anchored)",
        (true, false) => " (spec attested)",
        (false, true) => " (attestation anchored)",
        (false, false) => "",
    }
}
