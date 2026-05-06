//! Scene DSL — load `.scene` files, parse them, run them against the
//! recorder library to produce an asciinema cast.
//!
//! See `docs/scene-grammar.md` for the v1 grammar specification.

mod ast;
mod exec;
mod lex;
mod parse;

pub use ast::{Action, Config, Located, Scene, SpawnTarget};
pub use parse::parse;

use std::path::Path;

use anyhow::Context;

impl Scene {
    /// Read a scene file from disk and parse it.
    ///
    /// # Errors
    /// IO error reading the file, or any parse error annotated with
    /// the scene path and line number.
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("scene: read {}", path.display()))?;
        parse(&source).with_context(|| format!("scene: parse {}", path.display()))
    }
}
