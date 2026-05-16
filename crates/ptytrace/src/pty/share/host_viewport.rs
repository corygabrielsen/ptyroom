//! Host-side viewport renderer for `ptyroom host`.
//!
//! When the host enables local-output tee AND its stdout is a tty,
//! `ptyroom host` switches to a viewport view: the shared PTY's
//! contents render in the main pane and a status bar at the bottom
//! shows addr, command, client count, queue depth, and control hint.
//! This module owns the bar composition + the wrapper around the
//! generic `ViewportRenderer`.
//!
//! Non-viewport hosts (no local output, or piped stdout) bypass this
//! module entirely; their PTY output is teed via the simpler
//! `child_output_cleanup_guard` path in `terminal_state`.

use super::super::input_router::{LOCAL_ESCAPE_NAME, LocalStatus};
use super::super::room_protocol::TerminalSize;
use super::super::status_bar::{Bar, Chip};
use super::super::viewport::ViewportRenderer;

pub(super) struct HostViewport {
    inner: ViewportRenderer,
    addr: String,
    command: String,
    client_count: usize,
    queue_depth: usize,
    status: LocalStatus,
    controls: bool,
}

impl HostViewport {
    pub(super) fn enter(stdout_fd: i32, addr: String, command: String) -> anyhow::Result<Self> {
        let bar = build_host_bar(&addr, &command, 0, 0, LocalStatus::Connected, false);
        let title = format!("ptyroom host {addr}");
        let inner = ViewportRenderer::enter(stdout_fd, &title, &bar)?;
        Ok(Self {
            inner,
            addr,
            command,
            client_count: 0,
            queue_depth: 0,
            status: LocalStatus::Connected,
            controls: false,
        })
    }

    pub(super) fn set_controls_enabled(&mut self, enabled: bool) {
        self.controls = enabled;
    }

    pub(super) fn process_output(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.inner.process_output(bytes, &self.bar())
    }

    pub(super) fn resize(&mut self, stdout_fd: i32, size: TerminalSize) -> anyhow::Result<()> {
        self.inner.resize(stdout_fd, size, &self.bar())
    }

    pub(super) fn set_client_count(&mut self, count: usize) -> anyhow::Result<()> {
        if self.client_count == count {
            return Ok(());
        }
        self.client_count = count;
        self.inner.redraw_status(&self.bar())
    }

    pub(super) fn set_queue_depth(&mut self, depth: usize) -> anyhow::Result<()> {
        if self.queue_depth == depth {
            return Ok(());
        }
        self.queue_depth = depth;
        self.inner.redraw_status(&self.bar())
    }

    pub(super) fn set_status(
        &mut self,
        _stdout_fd: i32,
        status: LocalStatus,
    ) -> anyhow::Result<()> {
        self.status = status;
        self.inner.redraw_status(&self.bar())
    }

    pub(super) fn force_redraw(&mut self, stdout_fd: i32) -> anyhow::Result<()> {
        self.inner.force_redraw(stdout_fd, &self.bar())
    }

    pub(super) fn reported_size(stdout_fd: i32) -> Option<TerminalSize> {
        ViewportRenderer::reported_size(stdout_fd)
    }

    fn bar(&self) -> Bar {
        build_host_bar(
            &self.addr,
            &self.command,
            self.client_count,
            self.queue_depth,
            self.status,
            self.controls,
        )
    }
}

/// Compose the host's status bar from current state. Lives outside
/// `HostViewport` so unit tests can call it without owning a
/// `ViewportRenderer` (a real tty).
pub(super) fn build_host_bar(
    addr: &str,
    command: &str,
    client_count: usize,
    queue_depth: usize,
    status: LocalStatus,
    controls: bool,
) -> Bar {
    let clients_segment = match client_count {
        0 => "0 clients".to_string(),
        1 => "1 client".to_string(),
        n => format!("{n} clients"),
    };
    let mut bar = Bar::new(Chip::Host).segment(addr);
    if !command.is_empty() {
        bar = bar.segment(command);
    }
    bar = bar.segment(clients_segment);
    if queue_depth > 0 {
        bar = bar.segment(format!("{queue_depth} queued"));
    }
    match status {
        LocalStatus::Connected => {
            if controls {
                bar = bar.segment(format!("{LOCAL_ESCAPE_NAME} ? help"));
            }
        }
        LocalStatus::Command => {
            bar = bar
                .segment("command")
                .segment(". end")
                .segment("? help")
                .segment("r redraw")
                .segment(format!("{LOCAL_ESCAPE_NAME} send"));
        }
        LocalStatus::Help => {
            bar = bar
                .segment("controls")
                .segment(format!("{LOCAL_ESCAPE_NAME} . end"))
                .segment(format!("{LOCAL_ESCAPE_NAME} r redraw"))
                .segment(format!(
                    "{LOCAL_ESCAPE_NAME} {LOCAL_ESCAPE_NAME} send {LOCAL_ESCAPE_NAME}"
                ));
        }
    }
    bar
}
