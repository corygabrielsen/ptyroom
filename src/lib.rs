//! Deterministic terminal-session tracer for scripted and live CLI recording.
//!
//! Headline API: [`render`] takes a trace file path and returns a
//! [`Render`] builder that produces an MP4 or GIF in one chained call.
//!
//! Layered modules, bottom-up:
//! - [`color`] — `HexColor`, `CellColor`, `PaletteOverrides`.
//! - [`frame`] — per-frame terminal state with a rectangular grid invariant.
//! - [`trace`] — asciinema-shaped event log (the central artifact).
//! - [`frame_replay`] — trace → per-frame frames via vt100.
//! - [`paint`] — frames → PNG images.
//! - [`encode`] — PNG sequence → MP4/GIF via ffmpeg.
//! - [`inspect`] — ASCII rendering of a frame for debugging.
//! - [`tracer`] — spawn any interactive argv under a PTY and drive it.
//! - [`verify`] — load + diff frame directories.
//!
//! Determinism is exposed externally via [`witness`] (reproducibility
//! witnesses that re-render and compare hashes) and [`contract`]
//! (behavioral predicates over the trace). Predicate checks run inline
//! at record time.

pub mod color;
pub mod contract;
pub mod encode;
pub mod frame;
pub mod frame_replay;
pub mod inspect;
pub mod observer;
pub mod paint;
pub mod recording;
mod render;
pub mod script;
pub mod trace;
pub mod tracer;
pub mod verify;
pub mod witness;

pub use render::{Render, render};
