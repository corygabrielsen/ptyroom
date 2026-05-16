//! Deterministic rendering for PTY trace artifacts.
//!
//! This crate owns the `Trace -> Media + Witness` half of the command
//! algebra. It depends on [`ptytrace`] for the trace schema, provenance
//! anchors, contracts, and PTY color defaults; replay, frame snapshots,
//! painting, encoding, and witness verification live here.

pub mod encode;
pub mod frame;
pub mod frame_replay;
pub mod inspect;
pub mod paint;
mod render;
/// CLI dispatcher for the `ptyrender` binary.
///
/// Public for the binary target's sake (Rust can't make a module
/// `pub(crate)` and have a same-package `bin/` target reach it via
/// `crate_name::`). External consumers should use the programmatic
/// API ([`Render`] / [`render`]). Hidden from rustdoc because the
/// argument shape is binary-specific and subject to change.
#[doc(hidden)]
pub mod render_cli;
pub mod verify;
pub mod witness;

pub use ptytrace::{attestation, color, contract, observer, trace};
pub use render::{Render, render};
