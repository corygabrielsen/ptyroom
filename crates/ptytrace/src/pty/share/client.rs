//! Connected-client state for a `ptyroom` host.
//!
//! Each TCP connection becomes a [`Client`]:
//!
//!   - non-blocking `stream` for tee'd PTY output + client input
//!   - `pending` ring of bytes waiting to be flushed to the client
//!   - `input_pending` buffer for the room-protocol parser (handles
//!     control frames + raw bytes interleaved on the same stream)
//!
//! [`JoinReplay`] is the bounded ring of recently-broadcast frames
//! the host keeps so a late-joining client sees the current screen
//! state, not just bytes that arrive after their connection.
//!
//! [`ShareStats`] (broadcast outcome counters) + the free
//! [`broadcast`] / [`broadcast_control`] helpers live here too —
//! they exist *because* of the per-client backlog/disconnect rules
//! defined above. Session owns a `ShareStats` value but mutating it
//! is this module's job.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::{AsRawFd, RawFd};

use bytes::{Buf, Bytes};
use nix::poll::PollFlags;

use super::super::room_protocol::{self, ClientControl, TerminalSize};

/// Maximum bytes the host will buffer per client before disconnecting
/// for backlog. Bound on memory consumption per slow consumer.
pub(super) const MAX_CLIENT_BACKLOG_BYTES: usize = 1024 * 1024;

/// Maximum bytes the join-replay ring keeps. Older frames are
/// dropped when this is exceeded; a late joiner sees up to this
/// much recent history.
pub(super) const MAX_JOIN_REPLAY_BYTES: usize = 256 * 1024;

#[derive(Debug, Default)]
pub(super) struct JoinReplay {
    frames: VecDeque<Bytes>,
    bytes: usize,
}

impl JoinReplay {
    pub(super) fn remember(&mut self, frame: Bytes) {
        self.bytes = self.bytes.saturating_add(frame.len());
        self.frames.push_back(frame);
        while self.bytes > MAX_JOIN_REPLAY_BYTES {
            let Some(dropped) = self.frames.pop_front() else {
                self.bytes = 0;
                break;
            };
            self.bytes = self.bytes.saturating_sub(dropped.len());
        }
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn frames(&self) -> impl Iterator<Item = &Bytes> {
        self.frames.iter()
    }
}

#[derive(Debug)]
pub(super) struct Client {
    stream: TcpStream,
    /// Outbound frames waiting to flush to the client. Each entry is
    /// a refcounted `Bytes` slice shared with every other client that
    /// received the same broadcast — fan-out is a refcount bump, not
    /// a memcpy. Partial writes advance the head `Bytes` in place via
    /// [`Bytes::advance`]; a fully-written head is popped.
    pending: VecDeque<Bytes>,
    /// Total bytes queued across `pending`. Tracked explicitly because
    /// `VecDeque<Bytes>::len` reports frame count, not byte count, and
    /// the backlog cap is byte-denominated.
    pending_bytes: usize,
    input_pending: Vec<u8>,
    /// `true` while the client's input stream is still readable.
    /// Flipped to `false` when the peer closes its write half or a
    /// read returns 0. `pub(super)` so the share loop in mod.rs can
    /// both read the flag (when deciding whether to poll the fd)
    /// and write it (on `drain_input_to_pty` returning false).
    pub(super) input_open: bool,
    protocol_ready: bool,
    /// Last terminal size the client reported (via room-protocol
    /// [`ClientControl::Resize`]). `None` until the first resize
    /// frame arrives. Read by the size-sync helper in
    /// `share/mod.rs` to compute the canonical PTY dimensions.
    /// `pub(super)` for the same reason as `input_open`; tests in
    /// mod.rs also poke it to construct synthetic clients.
    pub(super) size: Option<TerminalSize>,
}

impl Client {
    pub(super) fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            pending: VecDeque::new(),
            pending_bytes: 0,
            input_pending: Vec::new(),
            input_open: true,
            protocol_ready: false,
            size: None,
        })
    }

    pub(super) fn poll_flags(&self) -> PollFlags {
        let mut flags = PollFlags::empty();
        if self.input_open {
            flags |= PollFlags::POLLIN;
        }
        if self.has_pending() {
            flags |= PollFlags::POLLOUT;
        }
        flags
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Snapshot of all currently-queued bytes, concatenated. Test-only
    /// helper for the replay-frame-boundary assertion; production code
    /// writes each `Bytes` frame directly in `flush_pending` without
    /// collecting.
    #[cfg(test)]
    pub(super) fn pending_snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pending_bytes);
        for frame in &self.pending {
            out.extend_from_slice(frame);
        }
        out
    }

    pub(super) fn enqueue(&mut self, frame: Bytes) -> bool {
        if frame.len() > MAX_CLIENT_BACKLOG_BYTES.saturating_sub(self.pending_bytes) {
            return false;
        }
        self.pending_bytes = self.pending_bytes.saturating_add(frame.len());
        self.pending.push_back(frame);
        true
    }

    pub(super) fn enqueue_replay(&mut self, replay: &JoinReplay) -> bool {
        if replay.bytes() > MAX_CLIENT_BACKLOG_BYTES.saturating_sub(self.pending_bytes) {
            return false;
        }
        for frame in replay.frames() {
            self.pending_bytes = self.pending_bytes.saturating_add(frame.len());
            self.pending.push_back(frame.clone());
        }
        true
    }

    pub(super) fn flush_pending(&mut self) -> bool {
        while let Some(head) = self.pending.front_mut() {
            if head.is_empty() {
                self.pending.pop_front();
                continue;
            }
            match self.stream.write(head) {
                Ok(0) => return false,
                Ok(n) => {
                    head.advance(n);
                    self.pending_bytes = self.pending_bytes.saturating_sub(n);
                    if head.is_empty() {
                        self.pending.pop_front();
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return true,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }

    pub(super) fn drain_input_to_pty(&mut self, pty_fd: i32) -> anyhow::Result<bool> {
        let mut buf = [0_u8; 4096];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    self.flush_pending_input_as_raw(pty_fd)?;
                    return Ok(false);
                }
                Ok(n) => {
                    self.input_pending.extend_from_slice(&buf[..n]);
                    if !self.flush_pending_input(pty_fd)? {
                        return Ok(false);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }
    }

    fn flush_pending_input(&mut self, pty_fd: i32) -> anyhow::Result<bool> {
        loop {
            if self.input_pending.is_empty() {
                return Ok(true);
            }
            let Some(start) =
                room_protocol::find_subslice(&self.input_pending, room_protocol::PREFIX)
            else {
                if !self.protocol_ready {
                    let keep =
                        room_protocol::prefix_overlap(&self.input_pending, room_protocol::PREFIX);
                    return Ok(
                        keep > 0 && self.input_pending.len() <= room_protocol::MAX_CONTROL_BYTES
                    );
                }
                let keep =
                    room_protocol::prefix_overlap(&self.input_pending, room_protocol::PREFIX);
                let write_len = self.input_pending.len().saturating_sub(keep);
                if write_len > 0 {
                    super::write_all(pty_fd, &self.input_pending[..write_len])?;
                    self.input_pending.drain(..write_len);
                }
                return Ok(true);
            };
            if start > 0 {
                if !self.protocol_ready {
                    return Ok(false);
                }
                super::write_all(pty_fd, &self.input_pending[..start])?;
                self.input_pending.drain(..start);
                continue;
            }

            let suffix_search_start = room_protocol::PREFIX.len();
            let Some(end_rel) = room_protocol::find_subslice(
                &self.input_pending[suffix_search_start..],
                room_protocol::SUFFIX,
            ) else {
                if self.input_pending.len() > room_protocol::MAX_CONTROL_BYTES {
                    if !self.protocol_ready {
                        return Ok(false);
                    }
                    super::write_all(pty_fd, &self.input_pending[..1])?;
                    self.input_pending.drain(..1);
                    continue;
                }
                return Ok(true);
            };
            let payload_start = room_protocol::PREFIX.len();
            let payload_end = suffix_search_start + end_rel;
            let payload = self.input_pending[payload_start..payload_end].to_vec();
            if !self.apply_control(&payload) {
                return Ok(false);
            }
            self.input_pending
                .drain(..payload_end + room_protocol::SUFFIX.len());
        }
    }

    fn flush_pending_input_as_raw(&mut self, pty_fd: i32) -> anyhow::Result<()> {
        if !self.protocol_ready {
            self.input_pending.clear();
            return Ok(());
        }
        if !self.input_pending.is_empty() {
            super::write_all(pty_fd, &self.input_pending)?;
            self.input_pending.clear();
        }
        Ok(())
    }

    fn apply_control(&mut self, payload: &[u8]) -> bool {
        match room_protocol::parse_client_control(payload) {
            Some(ClientControl::Hello(version)) => {
                if version == room_protocol::VERSION {
                    self.protocol_ready = true;
                    return true;
                }
                false
            }
            Some(ClientControl::Resize(size)) if self.protocol_ready => {
                self.size = Some(size);
                true
            }
            // Resize-before-hello disconnects the client. A malformed
            // control frame after a successful hello is not a benign
            // re-send; treat it the same and disconnect. Pre-fix the
            // `None` arm returned `self.protocol_ready`, silently
            // dropping any frame the parser couldn't decode.
            Some(ClientControl::Resize(_)) | None => false,
        }
    }

    pub(super) fn disconnect(&mut self) {
        // Clear the reported size so any future code path that
        // pools or reuses `Client` records can't inherit a stale
        // value into the next session's `desired_session_size` fold.
        // Today disconnected clients are dropped from the vec on the
        // spot, so this is defense-in-depth — but free.
        self.size = None;
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Raw fd for `poll()` registration. Encapsulates the
    /// underlying `TcpStream` so mod.rs doesn't reach for the
    /// stream type directly (only the fd is needed for the share
    /// loop's poll).
    pub(super) fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

/// Broadcast outcome counters owned by `Session` and mutated by
/// [`broadcast`]/[`broadcast_control`] every time a tee'd frame
/// reaches the client fan-out.
#[derive(Debug, Default)]
pub(super) struct ShareStats {
    pub(super) accepted: usize,
    pub(super) disconnected: usize,
    pub(super) dropped_for_backlog: usize,
}

/// Fan `frame` out to every connected client. Clients whose backlog
/// exceeds the per-client budget are dropped on the spot (counter:
/// `dropped_for_backlog`); clients whose stream write fails are
/// dropped (`disconnected`). Surviving clients stay in `clients`.
///
/// The `Bytes` argument is refcount-cloned per client; the underlying
/// payload allocation is shared across the fan-out instead of being
/// memcpy'd N times.
//
// Take `frame` by value so callers move their last reference into the
// fan-out — the per-client `clone()` inside the loop is then the only
// refcount bump that survives, and a single-client broadcast does zero
// extra clones. Clippy's `needless_pass_by_value` doesn't see that the
// move-then-clone pattern is intentional here.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn broadcast(clients: &mut Vec<Client>, frame: Bytes, stats: &mut ShareStats) {
    let mut kept = Vec::with_capacity(clients.len());
    for mut client in clients.drain(..) {
        if !client.enqueue(frame.clone()) {
            client.disconnect();
            stats.disconnected += 1;
            stats.dropped_for_backlog += 1;
        } else if client.flush_pending() {
            kept.push(client);
        } else {
            client.disconnect();
            stats.disconnected += 1;
        }
    }
    *clients = kept;
}

/// Fan out a control frame (room-protocol size or hello). Same
/// semantics as [`broadcast`] — kept as a separate name for
/// caller-side clarity.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn broadcast_control(clients: &mut Vec<Client>, frame: Bytes, stats: &mut ShareStats) {
    broadcast(clients, frame, stats);
}
