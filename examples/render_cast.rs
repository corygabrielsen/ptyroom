//! Headline API example: turn a recorded asciinema cast into a media file.
//!
//! Usage: `cargo run --release --example render_cast -- <cast> <out.mp4|out.gif>`

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let cast: PathBuf = args.next().expect("usage: render_cast <cast> <out>").into();
    let out: PathBuf = args.next().expect("usage: render_cast <cast> <out>").into();

    term_recorder::render(&cast)?
        .font_size(40.0)
        .to_path(&out)?;

    println!("wrote {}", out.display());
    Ok(())
}
