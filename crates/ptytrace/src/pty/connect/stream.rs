//! Host-to-join framing decoder.

use bytes::{Bytes, BytesMut};

use super::super::room_protocol::{self, ServerControl, TerminalSize};

/// One semantic event decoded from a ptyroom host's TCP stream.
///
/// External bridges (e.g. `ptyweb`) consume these to forward PTY
/// output and geometry changes to their own clients. The output
/// payload is a refcounted `Bytes` carved from the decoder's read
/// buffer via [`BytesMut::split_to`], so the buffer-to-event path is
/// zero-copy.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServerEvent {
    Hello(u16),
    Output(Bytes),
    Size(TerminalSize),
}

/// Stateful decoder that turns a host's raw byte stream into
/// [`ServerEvent`] values.
///
/// Push bytes as they arrive; the parser tracks partial frames
/// across boundaries.
///
/// `pending` is a `BytesMut` so frames can be carved out via
/// [`BytesMut::split_to`] without copying — the parser-to-event path
/// stays zero-copy even on heavy traffic.
#[derive(Debug, Default)]
pub struct ServerStream {
    pending: BytesMut,
    pending_data_len: Option<usize>,
}

impl ServerStream {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<ServerEvent> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        self.drain(&mut events);
        events
    }

    fn drain(&mut self, events: &mut Vec<ServerEvent>) {
        loop {
            if let Some(len) = self.pending_data_len {
                // Zero-length data frame: emit nothing, clear the
                // pending-length marker, and re-enter the drain so a
                // subsequent control frame already in the buffer is
                // processed in the same call instead of waiting on a
                // later push(). Handled explicitly rather than relying
                // on the `if len > 0` skip below — the explicit branch
                // documents the protocol contract (data;0 is a no-op
                // frame, not a stall).
                if len == 0 {
                    self.pending_data_len = None;
                    continue;
                }
                if self.pending.len() < len {
                    return;
                }
                events.push(ServerEvent::Output(self.pending.split_to(len).freeze()));
                self.pending_data_len = None;
                continue;
            }
            if self.pending.is_empty() {
                return;
            }
            // BytesMut is already contiguous; index directly.
            let Some(start) = room_protocol::find_subslice(&self.pending, room_protocol::PREFIX)
            else {
                let keep = room_protocol::prefix_overlap(&self.pending, room_protocol::PREFIX);
                let output_len = self.pending.len().saturating_sub(keep);
                if output_len > 0 {
                    events.push(ServerEvent::Output(
                        self.pending.split_to(output_len).freeze(),
                    ));
                }
                return;
            };
            if start > 0 {
                events.push(ServerEvent::Output(self.pending.split_to(start).freeze()));
                continue;
            }

            let suffix_search_start = room_protocol::PREFIX.len();
            let Some(end_rel) = room_protocol::find_subslice(
                &self.pending[suffix_search_start..],
                room_protocol::SUFFIX,
            ) else {
                if self.pending.len() > room_protocol::MAX_CONTROL_BYTES {
                    events.push(ServerEvent::Output(self.pending.split_to(1).freeze()));
                    continue;
                }
                return;
            };
            let payload_start = room_protocol::PREFIX.len();
            let payload_end = suffix_search_start + end_rel;
            let frame_end = payload_end + room_protocol::SUFFIX.len();
            // Decide what kind of frame this is *before* splitting it
            // off the pending buffer, so an unknown-control fallback
            // can still hand the original framed bytes through as
            // Output without re-allocating the prefix/suffix wrapper.
            let control =
                room_protocol::parse_server_control(&self.pending[payload_start..payload_end]);
            let frame = self.pending.split_to(frame_end).freeze();
            match control {
                Some(ServerControl::Hello(version)) => events.push(ServerEvent::Hello(version)),
                Some(ServerControl::Size(size)) => events.push(ServerEvent::Size(size)),
                Some(ServerControl::Data(len)) => {
                    self.pending_data_len = Some(len);
                }
                None => events.push(ServerEvent::Output(frame)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::{ServerEvent, ServerStream};
    use crate::pty::room_protocol::{self, TerminalSize};

    #[test]
    fn size_control_is_filtered_from_output() {
        let mut stream = ServerStream::default();

        assert_eq!(
            stream.push(b"before\x1bPptyroom;size;40;10\x1b\\after"),
            vec![
                ServerEvent::Output(Bytes::from_static(b"before")),
                ServerEvent::Size(TerminalSize { cols: 40, rows: 10 }),
                ServerEvent::Output(Bytes::from_static(b"after")),
            ]
        );
    }

    #[test]
    fn hello_control_is_reported() {
        let mut stream = ServerStream::default();

        assert_eq!(
            stream.push(&room_protocol::encode_hello_control()),
            vec![ServerEvent::Hello(1)]
        );
    }

    #[test]
    fn control_parser_handles_split_frames() {
        let mut stream = ServerStream::default();

        assert_eq!(
            stream.push(b"hello\x1bPpty"),
            vec![ServerEvent::Output(Bytes::from_static(b"hello")),]
        );
        assert_eq!(stream.push(b"room;size;80;24"), Vec::new());
        assert_eq!(
            stream.push(b"\x1b\\world"),
            vec![
                ServerEvent::Size(TerminalSize { cols: 80, rows: 24 }),
                ServerEvent::Output(Bytes::from_static(b"world")),
            ]
        );
    }

    #[test]
    fn data_frame_emits_control_lookalike_bytes_as_output() {
        let mut stream = ServerStream::default();
        let payload = b"before\x1bPptyroom;size;1;1\x1b\\after";
        let frame = room_protocol::encode_output_frame(payload);

        assert_eq!(
            stream.push(&frame),
            vec![ServerEvent::Output(Bytes::copy_from_slice(payload))]
        );
    }

    #[test]
    fn data_frame_handles_split_payload() {
        let mut stream = ServerStream::default();
        let mut frame = room_protocol::encode_output_frame(b"abcdef");
        let tail = frame.split_off(frame.len() - 2);

        assert_eq!(stream.push(&frame), Vec::new());
        assert_eq!(
            stream.push(&tail),
            vec![ServerEvent::Output(Bytes::from_static(b"abcdef"))]
        );
    }

    #[test]
    fn zero_length_data_frame_does_not_stall_following_output() {
        let mut stream = ServerStream::default();
        let mut frames = room_protocol::encode_output_frame(b"");
        frames.extend_from_slice(&room_protocol::encode_output_frame(b"next"));

        assert_eq!(
            stream.push(&frames),
            vec![ServerEvent::Output(Bytes::from_static(b"next"))]
        );
    }

    /// A `data;0` frame must not stall a subsequent control frame on
    /// the same buffer. Regression guard: if the drain loop ever stops
    /// re-entering after consuming the zero-length data marker, the
    /// size frame here would be left in `pending` until another push.
    #[test]
    fn zero_length_data_frame_does_not_stall_following_size_control() {
        let mut stream = ServerStream::default();
        let mut frames = room_protocol::encode_output_frame(b"");
        frames.extend_from_slice(&room_protocol::encode_size_control(TerminalSize {
            cols: 120,
            rows: 40,
        }));

        assert_eq!(
            stream.push(&frames),
            vec![ServerEvent::Size(TerminalSize {
                cols: 120,
                rows: 40
            })]
        );
    }

    #[test]
    fn unknown_control_frame_is_preserved_as_output() {
        let mut stream = ServerStream::default();
        let frame = b"\x1bPptyroom;unknown;field\x1b\\";

        assert_eq!(
            stream.push(frame),
            vec![ServerEvent::Output(Bytes::copy_from_slice(frame))]
        );
    }

    #[test]
    fn malformed_data_length_frame_is_preserved_as_output() {
        let mut stream = ServerStream::default();
        let frame = b"\x1bPptyroom;data;bogus\x1b\\";

        assert_eq!(
            stream.push(frame),
            vec![ServerEvent::Output(Bytes::copy_from_slice(frame))]
        );
    }
}
