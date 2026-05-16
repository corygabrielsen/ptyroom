//! `run` subcommand: run a `.script` file and write the resulting trace.

use std::path::PathBuf;

use ptytrace::script::Script;

#[derive(clap::Args)]
pub struct Args {
    /// Input script file.
    script: PathBuf,
    /// Output trace path.
    #[arg(long)]
    out: PathBuf,
}

pub fn run(args: &Args) -> anyhow::Result<()> {
    let script = Script::read(&args.script)?;
    let trace = script.run()?;

    if is_media_ext(&args.out) {
        anyhow::bail!(
            "ptytrace run writes traces only; choose a .ptytrace output, then run `ptyrender <trace.ptytrace> {}`",
            args.out.display()
        );
    }

    trace.write(&args.out)?;
    println!(
        "wrote {} ({} bytes, {} events)",
        args.out.display(),
        std::fs::metadata(&args.out)?.len(),
        trace.events.len()
    );
    Ok(())
}

fn is_media_ext(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");
    matches!(ext, "gif" | "mp4")
}
