//! Deterministic GIF recorder for tint demos.
//!
//! Layered, bottom-up:
//! - [`color`] — `HexColor`, `CellColor`, `PaletteOverrides`. Total parsers,
//!   no panics, JSON round-tripping.
//! - [`snapshot`] — per-frame terminal state with a rectangular grid invariant.
//! - [`cast`] — asciinema v2 file format.
//!
//! - [`raw_log`] — append-only input/output evidence.
//! - [`verified_trace`] — replay-checked transitions over deterministic
//!   observers.
//! - [`proof_timeline`] — verified transitions compiled to monotonic
//!   presentation time.
//!
//! Higher layers (paint / encode / inspect / verify / recorder / scenes)
//! depend only on these lower layers.

pub mod cast;
pub mod color;
pub mod contracts;
pub mod encode;
pub mod inspect;
pub mod observer;
pub mod paint;
pub mod proof;
pub mod proof_timeline;
pub mod raw_log;
pub mod recorder;
pub mod recording;
pub mod scenes;
pub mod snapshot;
pub mod timeline;
pub mod verified_trace;
pub mod verify;
