//! Deterministic terminal recorder for scripted CLI demos.
//!
//! Headline API: [`render`] takes a cast file path and returns a
//! [`Render`] builder that produces an MP4 or GIF in one chained call.
//!
//! Layered modules, bottom-up:
//! - [`color`] — `HexColor`, `CellColor`, `PaletteOverrides`.
//! - [`snapshot`] — per-frame terminal state with a rectangular grid invariant.
//! - [`cast`] — asciinema v2 file format.
//! - [`snapshot_replay`] — cast → per-frame snapshots via vt100.
//! - [`paint`] — snapshots → PNG frames.
//! - [`encode`] — PNG sequence → MP4/GIF via ffmpeg.
//! - [`inspect`] — ASCII rendering of a snapshot for debugging.
//! - [`recorder`] — spawn any interactive argv under a PTY and script it.
//! - [`verify`] — load + diff snapshot directories.
//!
//! Determinism is exposed externally via [`receipt`] (reproducibility
//! receipts that re-render and compare hashes). The previous internal
//! `Verified<T>` type-state machinery has been removed; predicate
//! checks now run inline at record time.

pub mod cast;
pub mod color;
pub mod encode;
pub mod inspect;
pub mod observer;
pub mod paint;
pub mod receipt;
pub mod recorder;
pub mod recording;
mod render;
pub mod scene;
pub mod snapshot;
pub mod snapshot_replay;
pub mod spec;
pub mod verify;

pub use render::{Render, render};
