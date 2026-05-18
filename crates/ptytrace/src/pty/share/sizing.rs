//! Terminal-size reconciliation for a ptyroom host.
//!
//! A shared PTY has many opinions on what size it should be: the host's
//! own terminal (when local output is on), the host viewport (when the
//! status bar is active), and every connected client's last reported
//! size. The PTY itself can only be one size at a time. This module
//! reduces those inputs to a single canonical size — currently the
//! per-axis minimum of all participants — and pushes changes out to
//! the PTY, the trace, the host viewport, and connected clients.
//!
//! `desired_session_size` is the policy; everything else is plumbing.
//! Keep them together so the policy is one read away from its callers.

use std::io;
use std::time::{Duration, Instant};

use anyhow::anyhow;

use super::super::process;
use super::super::room_protocol::{self, TerminalSize};
use super::super::terminal_io::terminal_size;
use super::client::{Client, ShareStats, broadcast_control};
use super::host_viewport::HostViewport;
use super::pending::{PendingEvent, PendingState};
use crate::recording::TraceBuilder;

pub(super) const SIZE_CHECK_INTERVAL: Duration = Duration::from_millis(250);

pub(super) fn sync_pty_size(
    pty: &mut process::PtyMaster,
    current: &mut TerminalSize,
    fallback: TerminalSize,
    host_size: Option<TerminalSize>,
    clients: &[Client],
) -> anyhow::Result<Option<TerminalSize>> {
    let desired = desired_session_size(fallback, host_size, clients);
    if desired == *current {
        return Ok(None);
    }
    pty.resize(desired.cols, desired.rows)
        .map_err(|err| anyhow!("resize shared PTY: {err}"))?;
    *current = desired;
    Ok(Some(desired))
}

pub(super) fn desired_session_size(
    fallback: TerminalSize,
    host_size: Option<TerminalSize>,
    clients: &[Client],
) -> TerminalSize {
    // A zero-valued axis means "I don't know this dimension yet,"
    // not "I want a zero-sized terminal." Filter per-axis before the
    // min fold so e.g. one client reporting (80, 0) and another
    // reporting (0, 24) compose to (80, 24) rather than collapsing
    // to (0, 0). When every participant has both axes unknown the
    // fold yields no contributors on that axis and we fall back.
    let sizes: Vec<TerminalSize> = host_size
        .into_iter()
        .chain(clients.iter().filter_map(|client| client.size))
        .collect();
    let min_axis = |selector: fn(TerminalSize) -> u16| -> Option<u16> {
        sizes
            .iter()
            .filter_map(|s| {
                let v = selector(*s);
                (v != 0).then_some(v)
            })
            .min()
    };
    TerminalSize {
        cols: min_axis(|s| s.cols).unwrap_or(fallback.cols),
        rows: min_axis(|s| s.rows).unwrap_or(fallback.rows),
    }
}

pub(super) fn refresh_host_size(
    local_output: bool,
    viewport_active: bool,
    stdout_fd: i32,
    host_size: &mut Option<TerminalSize>,
    last_size_check: &mut Instant,
) {
    if local_output && last_size_check.elapsed() >= SIZE_CHECK_INTERVAL {
        *host_size = if viewport_active {
            HostViewport::reported_size(stdout_fd)
        } else {
            terminal_size(stdout_fd)
        };
        *last_size_check = Instant::now();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn sync_canonical_size(
    pty: &mut process::PtyMaster,
    current_size: &mut TerminalSize,
    initial_size: TerminalSize,
    host_size: Option<TerminalSize>,
    clients: &mut Vec<Client>,
    host_viewport: Option<&mut HostViewport>,
    stdout_fd: i32,
    builder: &mut TraceBuilder,
    pending: &mut PendingState,
    stats: &mut ShareStats,
) -> anyhow::Result<()> {
    let Some(size) = sync_pty_size(pty, current_size, initial_size, host_size, clients)? else {
        return Ok(());
    };
    record_resize_event(builder, pending, size)?;
    broadcast_control(clients, &room_protocol::encode_size_control(size), stats);
    if let Some(viewport) = host_viewport {
        viewport.resize(stdout_fd, size)?;
    }
    Ok(())
}

pub(super) fn initial_pty_size(
    cols: u16,
    rows: u16,
    host_viewport: Option<&HostViewport>,
    stdout_fd: i32,
) -> TerminalSize {
    if host_viewport.is_some()
        && let Some(size) = HostViewport::reported_size(stdout_fd)
    {
        return size;
    }
    TerminalSize::new(cols, rows)
}

pub(super) fn initial_host_size(
    local_output: bool,
    stdout: &io::Stdout,
    stdout_fd: i32,
    host_viewport: Option<&HostViewport>,
) -> Option<TerminalSize> {
    use std::io::IsTerminal as _;
    if host_viewport.is_some() {
        HostViewport::reported_size(stdout_fd)
    } else if local_output && stdout.is_terminal() {
        terminal_size(stdout_fd)
    } else {
        None
    }
}

pub(super) fn record_resize_event(
    builder: &mut TraceBuilder,
    pending: &mut PendingState,
    size: TerminalSize,
) -> anyhow::Result<()> {
    // Resize is one of the two share-mode event sources, alongside
    // PTY output. Both feed the same pending buffer so the dwell on
    // any given event reflects the time-to-next-event regardless of
    // which kind comes next.
    pending.replace(
        PendingEvent::Resize {
            cols: size.cols,
            rows: size.rows,
        },
        Instant::now(),
        builder,
    )
}
