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

use std::io::BufRead;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use anyhow::Context;

use super::ctl_socket_path;

/// Maximum bytes for a single `add` payload. Lines exceeding this
/// are rejected to bound peer memory consumption.
pub(super) const CTL_MAX_PAYLOAD_BYTES: usize = 64 * 1024;

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
pub(super) fn parse_ctl_command<R: BufRead>(reader: &mut R) -> anyhow::Result<CtlCommand> {
    let mut line = String::new();
    reader.read_line(&mut line).context("read ctl command")?;
    let trimmed = line.trim_end_matches(['\n', '\r']);
    let mut parts = trimmed.splitn(2, ' ');
    let verb = parts.next().unwrap_or("");
    match verb {
        "add" => {
            let len_str = parts.next().context("add requires payload length")?;
            let len: usize = len_str.parse().context("invalid payload length")?;
            if len > CTL_MAX_PAYLOAD_BYTES {
                anyhow::bail!("payload too large (max {CTL_MAX_PAYLOAD_BYTES} bytes)");
            }
            let mut payload = vec![0_u8; len];
            reader
                .read_exact(&mut payload)
                .context("read ctl payload")?;
            let text = String::from_utf8(payload).context("payload is not valid UTF-8")?;
            Ok(CtlCommand::Queue(QueueOp::Add(text)))
        }
        "next" => Ok(CtlCommand::Queue(QueueOp::Next)),
        "list" => Ok(CtlCommand::Queue(QueueOp::List)),
        "clear" => Ok(CtlCommand::Queue(QueueOp::Clear)),
        other => anyhow::bail!("unknown control verb {other:?}"),
    }
}
