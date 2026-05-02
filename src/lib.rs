//! Deterministic GIF recorder for tint demos.
//!
//! Layered, bottom-up:
//! - [`color`] — `HexColor`, `CellColor`, `PaletteOverrides`. Total parsers,
//!   no panics, JSON round-tripping.
//! - [`snapshot`] — per-frame terminal state with a rectangular grid invariant.
//! - [`cast`] — asciinema v2 file format.
//!
//! Higher layers (paint / encode / inspect / verify / recorder / scenes)
//! depend only on what's below them.

pub mod color;
pub mod snapshot;
pub mod cast;
pub mod encode;
pub mod paint;
pub mod inspect;
pub mod verify;
pub mod contracts;
pub mod recorder;
pub mod scenes;
