//! Deterministic terminal recorder for scripted CLI demos.
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
//! Higher layers (paint / encode / inspect / verify / recorder) depend
//! only on these lower layers. The recorder core can spawn any
//! interactive argv; tint-specific scene helpers, contracts, and
//! pipeline orchestration live in the sibling `tint-recorder-scenes`
//! crate.

pub mod cast;
pub mod color;
pub mod encode;
pub mod inspect;
pub mod observer;
pub mod paint;
pub mod proof;
pub mod proof_timeline;
pub mod raw_log;
pub mod recorder;
pub mod recording;
pub mod snapshot;
pub mod timeline;
pub mod verified_trace;
pub mod verify;
