//! Host-to-join framing decoder.

use super::super::room_protocol::{self, ServerControl, TerminalSize};

/// One semantic event decoded from a ptyroom host's TCP stream.
///
/// External bridges (e.g. `ptyweb`) consume these to forward PTY
/// output and geometry changes to their own clients.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServerEvent {
    Hello(u16),
    Output(Vec<u8>),
    Size(TerminalSize),
}

/// Stateful decoder that turns a host's raw byte stream into
/// [`ServerEvent`] values.
///
/// Push bytes as they arrive; the parser tracks partial frames
/// across boundaries.
#[derive(Debug, Default)]
pub struct ServerStream {
    pending: Vec<u8>,
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
                if self.pending.len() < len {
                    return;
                }
                if len > 0 {
                    events.push(ServerEvent::Output(self.pending.drain(..len).collect()));
                }
                self.pending_data_len = None;
                continue;
            }
            if self.pending.is_empty() {
                return;
            }
            let Some(start) = room_protocol::find_subslice(&self.pending, room_protocol::PREFIX)
            else {
                let keep = room_protocol::prefix_overlap(&self.pending, room_protocol::PREFIX);
                let output_len = self.pending.len().saturating_sub(keep);
                if output_len > 0 {
                    events.push(ServerEvent::Output(
                        self.pending.drain(..output_len).collect(),
                    ));
                }
                return;
            };
            if start > 0 {
                events.push(ServerEvent::Output(self.pending.drain(..start).collect()));
                continue;
            }

            let suffix_search_start = room_protocol::PREFIX.len();
            let Some(end_rel) = room_protocol::find_subslice(
                &self.pending[suffix_search_start..],
                room_protocol::SUFFIX,
            ) else {
                if self.pending.len() > room_protocol::MAX_CONTROL_BYTES {
                    events.push(ServerEvent::Output(self.pending.drain(..1).collect()));
                    continue;
                }
                return;
            };
            let payload_start = room_protocol::PREFIX.len();
            let payload_end = suffix_search_start + end_rel;
            let frame_end = payload_end + room_protocol::SUFFIX.len();
            let payload = self.pending[payload_start..payload_end].to_vec();
            let frame = self.pending.drain(..frame_end).collect::<Vec<_>>();
            match room_protocol::parse_server_control(&payload) {
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
    use super::{ServerEvent, ServerStream};
    use crate::pty::room_protocol::{self, TerminalSize};

    #[test]
    fn size_control_is_filtered_from_output() {
        let mut stream = ServerStream::default();

        assert_eq!(
            stream.push(b"before\x1bPptyroom;size;40;10\x1b\\after"),
            vec![
                ServerEvent::Output(b"before".to_vec()),
                ServerEvent::Size(TerminalSize { cols: 40, rows: 10 }),
                ServerEvent::Output(b"after".to_vec()),
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
            vec![ServerEvent::Output(b"hello".to_vec()),]
        );
        assert_eq!(stream.push(b"room;size;80;24"), Vec::new());
        assert_eq!(
            stream.push(b"\x1b\\world"),
            vec![
                ServerEvent::Size(TerminalSize { cols: 80, rows: 24 }),
                ServerEvent::Output(b"world".to_vec()),
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
            vec![ServerEvent::Output(payload.to_vec())]
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
            vec![ServerEvent::Output(b"abcdef".to_vec())]
        );
    }

    #[test]
    fn zero_length_data_frame_does_not_stall_following_output() {
        let mut stream = ServerStream::default();
        let mut frames = room_protocol::encode_output_frame(b"");
        frames.extend_from_slice(&room_protocol::encode_output_frame(b"next"));

        assert_eq!(
            stream.push(&frames),
            vec![ServerEvent::Output(b"next".to_vec())]
        );
    }

    #[test]
    fn unknown_control_frame_is_preserved_as_output() {
        let mut stream = ServerStream::default();
        let frame = b"\x1bPptyroom;unknown;field\x1b\\";

        assert_eq!(
            stream.push(frame),
            vec![ServerEvent::Output(frame.to_vec())]
        );
    }

    #[test]
    fn malformed_data_length_frame_is_preserved_as_output() {
        let mut stream = ServerStream::default();
        let frame = b"\x1bPptyroom;data;bogus\x1b\\";

        assert_eq!(
            stream.push(frame),
            vec![ServerEvent::Output(frame.to_vec())]
        );
    }
}
