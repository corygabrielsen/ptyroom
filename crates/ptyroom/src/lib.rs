//! Shared-terminal room facade.
//!
//! This crate is the package boundary for `SharedPtySession -> Trace`.
//!
//! External bridges (e.g. the `ptyweb` browser bridge) that need to
//! speak the room wire protocol without owning a local terminal use
//! [`protocol`] for encoders/decoders and [`stream::ServerStream`] to
//! parse a host's TCP byte stream into semantic events.

pub use ptytrace::pty::connect::stream;
pub use ptytrace::pty::room_protocol as protocol;
pub use ptytrace::pty::{connect, share};
