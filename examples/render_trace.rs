//! Headline API example: turn a recorded trace into a media file.
//!
//! Usage: `cargo run --release --example render_trace -- <trace> <out.mp4|out.gif>`

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let trace: PathBuf = args
        .next()
        .expect("usage: render_trace <trace> <out>")
        .into();
    let out: PathBuf = args
        .next()
        .expect("usage: render_trace <trace> <out>")
        .into();

    ptytrace::render(&trace)?.font_size(40.0).to_path(&out)?;

    println!("wrote {}", out.display());
    Ok(())
}
