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
//! Internal-only modules (`proof_timeline`, `raw_log`, `verified_trace`)
//! support the verified-replay machinery and are reached only via the
//! public `recorder` and `recording` APIs.

pub mod cast;
pub mod color;
pub mod encode;
pub mod inspect;
pub mod observer;
pub mod paint;
pub mod proof;
pub(crate) mod proof_timeline;
pub(crate) mod raw_log;
pub mod receipt;
pub mod recorder;
pub mod recording;
mod render;
pub mod snapshot;
pub mod snapshot_replay;
pub mod timeline;
pub(crate) mod verified_trace;
pub mod verify;

pub use render::{Render, render};
