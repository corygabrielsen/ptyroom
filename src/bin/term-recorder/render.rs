//! `render` subcommand: cast file → MP4/GIF in one call, with optional
//! receipt and behavioral spec attestation.

use std::path::PathBuf;

use term_recorder::receipt::sha256_hex;

#[derive(clap::Args)]
pub struct Args {
    /// Input cast file (asciinema v2 JSONL).
    cast: PathBuf,
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
    /// matching spec via `term-recorder verify --spec`. Requires
    /// `--receipt` to be set.
    #[arg(long, requires = "receipt")]
    spec: Option<PathBuf>,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let mut r = term_recorder::render(&args.cast)?
        .font_size(args.font_size)
        .padding(args.padding)
        .fps(args.fps);
    if let Some(w) = args.width {
        r = r.width(w);
    }
    if let Some(spec_path) = &args.spec {
        let spec_bytes = std::fs::read(spec_path)?;
        r = r.spec_sha256(sha256_hex(&spec_bytes));
    }

    if let Some(receipt_path) = &args.receipt {
        let receipt = r.to_path_with_receipt(&args.out)?;
        receipt.write(receipt_path)?;
        let suffix = if args.spec.is_some() {
            " (spec attested)"
        } else {
            ""
        };
        println!(
            "wrote {} + receipt {}{}",
            args.out.display(),
            receipt_path.display(),
            suffix,
        );
    } else {
        r.to_path(&args.out)?;
        println!("wrote {}", args.out.display());
    }
    Ok(())
}
