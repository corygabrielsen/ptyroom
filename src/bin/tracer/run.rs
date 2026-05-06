//! `record` subcommand: run a `.scene` file and write the resulting
//! cast (and optionally chain through render to MP4/GIF, with optional
//! receipt/spec attestation).

use std::path::PathBuf;

use tracer::script::Script;
use tracer::witness::sha256_hex;

#[derive(clap::Args)]
pub struct Args {
    /// Input scene file.
    scene: PathBuf,
    /// Output path. If extension is `.cast`, write the asciinema cast
    /// directly. Otherwise (e.g. `.mp4`, `.gif`) chain through render.
    #[arg(long)]
    out: PathBuf,
    /// Optional receipt path (only meaningful when --out is media,
    /// not a cast). The receipt embeds the cast hash and, if --spec
    /// is set, the spec hash.
    #[arg(long)]
    receipt: Option<PathBuf>,
    /// Optional behavioral spec to attest in the receipt and re-check.
    #[arg(long, requires = "receipt")]
    spec: Option<PathBuf>,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let scene = Script::read(&args.scene)?;
    let cast = scene.run()?;

    let ext = args
        .out
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");

    if ext == "cast" {
        if args.receipt.is_some() {
            anyhow::bail!("--receipt is meaningful only when --out is a media file (.mp4 / .gif)");
        }
        cast.write_with_summary(&args.out)?;
        println!("wrote {}", args.out.display());
        return Ok(());
    }

    // Chain through render: cast → media. Use a tempfile for the
    // intermediate cast so the user only sees the media output.
    let cast_tmp = tempfile::Builder::new()
        .prefix("tracer-script-")
        .suffix(".cast")
        .tempfile()?;
    cast.write(cast_tmp.path())?;

    let mut r = tracer::render(cast_tmp.path())?;
    if let Some(spec_path) = &args.spec {
        let spec_bytes = std::fs::read(spec_path)?;
        r = r.contract_sha256(sha256_hex(&spec_bytes));
    }
    // Always pin the scene as provenance when a receipt is requested —
    // the scene file IS the recipe, no flag needed.
    if args.receipt.is_some() {
        let scene_bytes = std::fs::read(&args.scene)?;
        r = r.script_sha256(sha256_hex(&scene_bytes));
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
