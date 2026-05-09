//! Shared-terminal room facade.
//!
//! This crate is the package boundary for `SharedPtySession -> Trace`.

pub use ptytrace::pty::{connect, share};
