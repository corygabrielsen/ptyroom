//! Unix-domain control socket for `ptyroom host` queue management.
//!
//! A host process binds a per-port socket under the resolved
//! runtime state directory (see [`super::resolve_state_dir`]); the
//! `ptyroom ctl <addr> queue ...` subcommand connects to it to
//! enqueue messages, inject the next one into the shared PTY, list
//! depth, or clear. The protocol is a single-line verb followed by
//! an optional length-prefixed payload.
//!
//! Wire format:
//!   - `add <len>\n<payload-of-len-bytes>`  — enqueue
//!   - `next\n`                              — inject next
//!   - `list\n`                              — depth report
//!   - `clear\n`                             — drop all
//!
//! Replies are short ASCII lines starting with `ok ` or `err `; the
//! caller (binary side) reads them back from the same stream.

use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use anyhow::Context;

use super::ctl_socket_path;

/// Maximum bytes for a single `add` payload. Lines exceeding this
/// are rejected to bound peer memory consumption.
pub(super) const CTL_MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Maximum bytes for the verb line (everything up to the first `\n`).
/// The longest legitimate verb is `add <len>\n` where `<len>` is a
/// decimal up to `CTL_MAX_PAYLOAD_BYTES` (5 digits) — under 16 bytes
/// total. 256 bytes is generous without giving a misbehaving peer
/// allocation headroom.
pub(super) const MAX_CTL_LINE_BYTES: usize = 256;

/// Owns the Unix-domain listener and removes the socket file on drop.
pub(super) struct CtlSocket {
    pub(super) listener: UnixListener,
    path: PathBuf,
}

impl CtlSocket {
    /// Bind a control socket for `port` under `state_dir`. Creates
    /// `state_dir` if missing (best-effort; default permissions).
    /// Returns `Err` if another process holds the path (the host treats
    /// this as non-fatal and runs without queue control).
    pub(super) fn bind(state_dir: &Path, port: u16) -> anyhow::Result<Self> {
        std::fs::create_dir_all(state_dir).with_context(|| {
            format!("create ptyroom state directory at {}", state_dir.display())
        })?;
        let path = ctl_socket_path(state_dir, port);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("bind ptyroom control socket at {}", path.display()))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path })
    }
}

impl Drop for CtlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One parsed control command. Outer variants are namespaces (nouns);
/// inner enums are the actions (verbs) — mirrors the
/// `ctl <noun> <action>` CLI convention documented in
/// `docs/ctl-protocol.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CtlCommand {
    Queue(QueueOp),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QueueOp {
    Add(String),
    Next,
    List,
    Clear,
}

/// Parse one verb + (optional) payload from `reader`. Returns
/// `Err` for malformed verbs, oversized payloads, or non-UTF-8
/// payloads.
pub(super) fn parse_ctl_command<R: Read>(reader: &mut R) -> anyhow::Result<CtlCommand> {
    // Verbs are tiny (`add <decimal>`, `next`, `list`, `clear`) — well
    // under `MAX_CTL_LINE_BYTES`. Read into a fixed-size byte buffer
    // up to the first newline and parse directly, avoiding the
    // `String + read_line` UTF-8 validation + heap allocation for the
    // verb line. The payload read below stays untouched; its size is
    // bounded separately by `CTL_MAX_PAYLOAD_BYTES`.
    let mut buf = [0u8; MAX_CTL_LINE_BYTES];
    let mut filled = 0usize;
    let mut saw_newline = false;
    while filled < buf.len() {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                buf[filled] = byte[0];
                filled += 1;
                if byte[0] == b'\n' {
                    saw_newline = true;
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(anyhow::Error::from(e).context("read ctl command")),
        }
    }
    if !saw_newline && filled == buf.len() {
        anyhow::bail!("ctl command line too long (max {MAX_CTL_LINE_BYTES} bytes)");
    }
    let line = &buf[..filled];
    // Strip the trailing CRLF and any incidental leading whitespace.
    // Pre-fix, `" add 5\nhello"` parsed the verb as empty and produced
    // an opaque "unknown control verb" error.
    let line = trim_start_ws(line);
    let line = trim_end_eol(line);
    let mut parts = line.splitn(2, |b| *b == b' ');
    let verb = parts.next().unwrap_or(&[]);
    match verb {
        b"add" => {
            let len_bytes = parts.next().context("add requires payload length")?;
            let len_str =
                std::str::from_utf8(len_bytes).context("invalid payload length encoding")?;
            let len: usize = len_str.parse().context("invalid payload length")?;
            if len > CTL_MAX_PAYLOAD_BYTES {
                anyhow::bail!("payload too large (max {CTL_MAX_PAYLOAD_BYTES} bytes)");
            }
            let payload = read_payload(reader, len)?;
            let text = String::from_utf8(payload).context("payload is not valid UTF-8")?;
            Ok(CtlCommand::Queue(QueueOp::Add(text)))
        }
        b"next" => Ok(CtlCommand::Queue(QueueOp::Next)),
        b"list" => Ok(CtlCommand::Queue(QueueOp::List)),
        b"clear" => Ok(CtlCommand::Queue(QueueOp::Clear)),
        other => {
            let lossy = String::from_utf8_lossy(other);
            anyhow::bail!("unknown control verb {lossy:?}")
        }
    }
}

/// Payload size above which we read into an uninitialized capacity
/// buffer chunk-by-chunk instead of pre-zeroing the whole `Vec`.
/// Below this threshold, the zero-fill is cheaper than the loop
/// bookkeeping; above it, the saved `memset` dominates.
const CTL_PAYLOAD_STREAM_THRESHOLD: usize = 16 * 1024;

fn read_payload<R: Read>(reader: &mut R, len: usize) -> anyhow::Result<Vec<u8>> {
    if len <= CTL_PAYLOAD_STREAM_THRESHOLD {
        let mut payload = vec![0_u8; len];
        reader
            .read_exact(&mut payload)
            .context("read ctl payload")?;
        return Ok(payload);
    }
    // Large payload path: allocate capacity once, then read 4 KiB at
    // a time into the uninitialized tail instead of zeroing the full
    // 64 KiB buffer up front. Uses the safe `chunk` + `extend_from_slice`
    // pattern to avoid `MaybeUninit` machinery.
    let mut payload: Vec<u8> = Vec::with_capacity(len);
    let mut chunk = [0_u8; 4 * 1024];
    while payload.len() < len {
        let want = (len - payload.len()).min(chunk.len());
        reader
            .read_exact(&mut chunk[..want])
            .context("read ctl payload")?;
        payload.extend_from_slice(&chunk[..want]);
    }
    Ok(payload)
}

fn trim_start_ws(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    &s[i..]
}

fn trim_end_eol(s: &[u8]) -> &[u8] {
    let mut end = s.len();
    while end > 0 && (s[end - 1] == b'\n' || s[end - 1] == b'\r') {
        end -= 1;
    }
    &s[..end]
}
