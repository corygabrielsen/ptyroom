//! Tint-specific layer over the generic `tint_recorder` library.
//!
//! Splits cleanly from the recorder so the library half can be reused
//! against any interactive process; this crate ships only the parts
//! that depend on the tint CLI:
//! - [`scenes`] — scene helpers (PROMPT bytes, picker patterns, themes).
//! - [`contracts`] — per-scene verify check registry.
//! - [`pipeline_test`] — end-to-end pipeline orchestration (record →
//!   snapshot → paint → encode → verify) for the tint scene set.

pub mod contracts;
pub mod pipeline_test;
pub mod scenes;
