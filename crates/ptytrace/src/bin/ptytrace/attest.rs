//! `attest` subcommand: produce detached provenance anchors for traces.

use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    provider: Provider,
}

#[derive(clap::Subcommand)]
enum Provider {
    /// Emit an unsigned local-file attestation over a trace hash.
    File(FileArgs),
}

#[derive(clap::Args)]
struct FileArgs {
    /// Trace file to anchor.
    #[arg(long)]
    trace: PathBuf,
    /// Attestation JSON output path.
    #[arg(long)]
    out: PathBuf,
    /// Issuer label to write into the attestation.
    #[arg(long)]
    issuer: Option<String>,
    /// Subject label to write into the attestation. Defaults to the trace filename.
    #[arg(long)]
    subject: Option<String>,
    /// Optional nonce to bind into the attestation freshness field.
    #[arg(long)]
    nonce: Option<String>,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    match &args.provider {
        Provider::File(file) => run_file(file),
    }
}

fn run_file(args: &FileArgs) -> anyhow::Result<()> {
    let (trace_sha256, trace_size_bytes) = ptytrace::attestation_io::trace_sha256(&args.trace)?;
    let attestation = ptytrace::attestation_io::file_attestation(
        &args.trace,
        &trace_sha256,
        trace_size_bytes,
        args.issuer.as_deref(),
        args.subject.as_deref(),
        args.nonce.as_deref(),
    )?;
    let attestation_sha256 = ptytrace::attestation_io::write_attestation(&args.out, &attestation)?;

    println!("wrote {}", args.out.display());
    println!("kind: {}", attestation.kind);
    println!("target_sha256: {}", short_hash(&trace_sha256));
    println!("attestation_sha256: {}", short_hash(&attestation_sha256));
    Ok(())
}

fn short_hash(hash: &str) -> &str {
    &hash[..hash.len().min(16)]
}
