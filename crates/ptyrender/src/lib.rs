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
#[doc(hidden)]
pub mod render_cli;
pub mod verify;
pub mod witness;

pub use ptytrace::{attestation, color, contract, observer, trace};
pub use render::{Render, render};
