//! Deterministic PTY session recording for scripted and live CLI workflows.
//!
//! Headline APIs:
//! - [`pty`] spawns, drives, captures, and shares interactive PTY sessions.
//! - [`script`] runs reproducible `.script` files into durable traces.
//! - [`trace`] defines the append-only artifact consumed by downstream tools.
//!
//! Layered modules, bottom-up:
//! - [`color`] — `HexColor`, `CellColor`, `PaletteOverrides`.
//! - [`trace`] — asciinema-shaped event log (the central artifact).
//! - [`attestation`] — provider claims that bind external provenance to a trace hash.
//! - [`pty`] — spawn any interactive argv under a PTY, capture it live,
//!   or share it with connected terminals.
//!
//! Rendering, frame replay, media encoding, and reproducibility witnesses
//! live in the sibling `ptyrender` crate. `ptytrace` keeps only the raw trace
//! and PTY mechanics that lower and sibling crates can safely depend on.

pub mod attestation;
#[doc(hidden)]
pub mod attestation_io;
pub mod color;
pub mod contract;
pub mod observer;
pub mod pty;
pub mod recording;
pub mod script;
pub mod trace;
