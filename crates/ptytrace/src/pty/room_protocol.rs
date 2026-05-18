//! Shared `ptyroom` wire framing.
//!
//! Host, join, and watch paths use this module so protocol names,
//! versioning, control parsing, and frame construction cannot drift
//! independently.
//!
//! The encoders and decoders here are also the public surface
//! external bridges (e.g. `ptyweb`) speak to a running ptyroom host
//! without re-implementing the framing.

pub const VERSION: u16 = 1;
pub const MAX_CONTROL_BYTES: usize = 1024;
const MAX_DATA_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const PREFIX: &[u8] = b"\x1bPptyroom;";
pub const SUFFIX: &[u8] = b"\x1b\\";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl TerminalSize {
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientControl {
    Hello(u16),
    Resize(TerminalSize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerControl {
    Hello(u16),
    Size(TerminalSize),
    Data(usize),
}

#[must_use]
pub fn encode_hello_control() -> Vec<u8> {
    encode_control(&format!("hello;{VERSION}"))
}

#[must_use]
pub fn encode_resize_control(size: TerminalSize) -> Vec<u8> {
    encode_control(&format!("resize;{};{}", size.cols, size.rows))
}

#[must_use]
pub fn encode_size_control(size: TerminalSize) -> Vec<u8> {
    encode_control(&format!("size;{};{}", size.cols, size.rows))
}

#[must_use]
pub fn encode_output_frame(bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(PREFIX.len() + 24 + SUFFIX.len() + bytes.len());
    frame.extend_from_slice(PREFIX);
    frame.extend_from_slice(format!("data;{}", bytes.len()).as_bytes());
    frame.extend_from_slice(SUFFIX);
    frame.extend_from_slice(bytes);
    frame
}

fn encode_control(payload: &str) -> Vec<u8> {
    let mut frame = Vec::with_capacity(PREFIX.len() + payload.len() + SUFFIX.len());
    frame.extend_from_slice(PREFIX);
    frame.extend_from_slice(payload.as_bytes());
    frame.extend_from_slice(SUFFIX);
    frame
}

#[must_use]
pub fn parse_client_control(payload: &[u8]) -> Option<ClientControl> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut parts = text.split(';');
    match parts.next()? {
        "hello" => {
            let version = parts.next()?.parse::<u16>().ok()?;
            if parts.next().is_some() {
                return None;
            }
            Some(ClientControl::Hello(version))
        }
        "resize" => {
            let cols = parts.next()?.parse::<u16>().ok()?;
            let rows = parts.next()?.parse::<u16>().ok()?;
            if cols == 0 || rows == 0 || parts.next().is_some() {
                return None;
            }
            Some(ClientControl::Resize(TerminalSize::new(cols, rows)))
        }
        _ => None,
    }
}

#[must_use]
pub fn parse_server_control(payload: &[u8]) -> Option<ServerControl> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut parts = text.split(';');
    match parts.next()? {
        "hello" => {
            let version = parts.next()?.parse::<u16>().ok()?;
            if parts.next().is_some() {
                return None;
            }
            Some(ServerControl::Hello(version))
        }
        "size" => {
            let cols = parts.next()?.parse::<u16>().ok()?;
            let rows = parts.next()?.parse::<u16>().ok()?;
            if cols == 0 || rows == 0 || parts.next().is_some() {
                return None;
            }
            Some(ServerControl::Size(TerminalSize::new(cols, rows)))
        }
        "data" => {
            let len = parts.next()?.parse::<usize>().ok()?;
            if len > MAX_DATA_FRAME_BYTES || parts.next().is_some() {
                return None;
            }
            Some(ServerControl::Data(len))
        }
        _ => None,
    }
}

#[must_use]
pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[must_use]
pub fn prefix_overlap(haystack: &[u8], prefix: &[u8]) -> usize {
    let max = haystack.len().min(prefix.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|&len| haystack[haystack.len() - len..] == prefix[..len])
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        ClientControl, MAX_DATA_FRAME_BYTES, ServerControl, TerminalSize, VERSION,
        encode_hello_control, encode_output_frame, encode_resize_control, encode_size_control,
        parse_client_control, parse_server_control,
    };

    #[test]
    fn hello_uses_current_version() {
        assert_eq!(encode_hello_control(), b"\x1bPptyroom;hello;1\x1b\\");
        assert_eq!(
            parse_client_control(format!("hello;{VERSION}").as_bytes()),
            Some(ClientControl::Hello(VERSION))
        );
        assert_eq!(
            parse_server_control(format!("hello;{VERSION}").as_bytes()),
            Some(ServerControl::Hello(VERSION))
        );
    }

    #[test]
    fn geometry_controls_round_trip() {
        let size = TerminalSize {
            cols: 100,
            rows: 30,
        };

        assert_eq!(
            encode_resize_control(size),
            b"\x1bPptyroom;resize;100;30\x1b\\"
        );
        assert_eq!(
            parse_client_control(b"resize;100;30"),
            Some(ClientControl::Resize(size))
        );
        assert_eq!(encode_size_control(size), b"\x1bPptyroom;size;100;30\x1b\\");
        assert_eq!(
            parse_server_control(b"size;100;30"),
            Some(ServerControl::Size(size))
        );
    }

    #[test]
    fn data_frames_are_length_delimited() {
        let payload = b"before\x1bPptyroom;size;1;1\x1b\\after";
        let expected = b"\x1bPptyroom;data;31\x1b\\before\x1bPptyroom;size;1;1\x1b\\after";

        assert_eq!(encode_output_frame(payload), expected);
        assert_eq!(
            parse_server_control(b"data;31"),
            Some(ServerControl::Data(31))
        );
    }

    #[test]
    fn geometry_controls_reject_zero_dimensions_and_extra_fields() {
        assert_eq!(parse_client_control(b"resize;0;24"), None);
        assert_eq!(parse_client_control(b"resize;80;0"), None);
        assert_eq!(parse_client_control(b"resize;80;24;extra"), None);
        assert_eq!(parse_server_control(b"size;0;24"), None);
        assert_eq!(parse_server_control(b"size;80;0"), None);
        assert_eq!(parse_server_control(b"size;80;24;extra"), None);
    }

    #[test]
    fn data_controls_reject_invalid_lengths() {
        assert_eq!(parse_server_control(b"data;bogus"), None);
        assert_eq!(parse_server_control(b"data;1;extra"), None);
        assert_eq!(
            parse_server_control(format!("data;{}", MAX_DATA_FRAME_BYTES + 1).as_bytes()),
            None
        );
    }
}
