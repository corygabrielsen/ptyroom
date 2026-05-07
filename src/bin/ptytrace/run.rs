//! `run` subcommand: run a `.script` file and write the resulting
//! trace (and optionally chain through render to MP4/GIF, with optional
//! receipt, spec, and provenance attestation).

use std::path::PathBuf;

use ptytrace::script::Script;
use ptytrace::witness::sha256_hex;

#[derive(clap::Args)]
pub struct Args {
    /// Input script file.
    script: PathBuf,
    /// Output path. If extension is `.ptytrace`, `.trace`, or `.cast`,
    /// write the trace directly. Otherwise (e.g. `.mp4`, `.gif`) chain
    /// through render.
    #[arg(long)]
    out: PathBuf,
    /// Optional receipt path (only meaningful when --out is media,
    /// not a trace). The receipt embeds the trace hash and, if --spec
    /// is set, the spec hash.
    #[arg(long)]
    receipt: Option<PathBuf>,
    /// Optional behavioral spec to attest in the receipt and re-check.
    #[arg(long, requires = "receipt")]
    spec: Option<PathBuf>,
    /// Existing attestation sidecar to hash into the receipt. The
    /// attestation must target the trace produced by this run.
    #[arg(long, requires = "receipt", conflicts_with = "attestation_out")]
    attestation: Option<PathBuf>,
    /// Write an unsigned local-file attestation for the trace produced
    /// by this run and hash it into the receipt.
    #[arg(long, requires = "receipt", conflicts_with = "attestation")]
    attestation_out: Option<PathBuf>,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let script = Script::read(&args.script)?;
    let trace = script.run()?;

    let ext = args
        .out
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");

    if is_trace_ext(ext) {
        if args.receipt.is_some() {
            anyhow::bail!("--receipt is meaningful only when --out is a media file (.mp4 / .gif)");
        }
        trace.write_with_summary(&args.out)?;
        println!("wrote {}", args.out.display());
        return Ok(());
    }

    // Chain through render: trace -> media. Use a tempfile for the
    // intermediate trace so the user only sees the media output.
    let trace_tmp = tempfile::Builder::new()
        .prefix("ptytrace-script-")
        .suffix(".ptytrace")
        .tempfile()?;
    trace.write(trace_tmp.path())?;
    let (trace_sha256, trace_size_bytes) = super::attestation_io::trace_sha256(trace_tmp.path())?;

    let mut r = ptytrace::render(trace_tmp.path())?;
    if let Some(spec_path) = &args.spec {
        let spec_bytes = std::fs::read(spec_path)?;
        r = r.contract_sha256(sha256_hex(&spec_bytes));
    }
    // Always pin the script as provenance when a receipt is requested:
    // the script file is the recipe, no flag needed.
    if args.receipt.is_some() {
        let script_bytes = std::fs::read(&args.script)?;
        r = r.script_sha256(sha256_hex(&script_bytes));
    }
    let mut attestation_written = None;
    if let Some(attestation_path) = &args.attestation {
        let attestation_sha256 =
            super::attestation_io::attestation_sha256_for_trace(attestation_path, &trace_sha256)?;
        r = r.attestation_sha256(attestation_sha256);
    } else if let Some(attestation_path) = &args.attestation_out {
        let attestation = super::attestation_io::file_attestation(
            trace_tmp.path(),
            &trace_sha256,
            trace_size_bytes,
            None,
            None,
            None,
        )?;
        let attestation_sha256 =
            super::attestation_io::write_attestation(attestation_path, &attestation)?;
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

fn is_trace_ext(ext: &str) -> bool {
    matches!(ext, "ptytrace" | "trace" | "cast")
}

fn receipt_suffix(has_spec: bool, has_attestation: bool) -> &'static str {
    match (has_spec, has_attestation) {
        (true, true) => " (spec + attestation anchored)",
        (true, false) => " (spec attested)",
        (false, true) => " (attestation anchored)",
        (false, false) => "",
    }
}
