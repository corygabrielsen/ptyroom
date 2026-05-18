//! Generic alt-screen viewport renderer for ptyroom modes.
//!
//! Owns alt-screen entry/exit, window-title set/restore, vt100 parsing
//! of PTY output, and bottom-row status-bar rendering. Mode-specific
//! state (`ptyroom join` / `watch` / `host`) lives in the caller; this
//! module is given a [`Bar`] on each redraw and renders what it is
//! told.

use std::os::fd::RawFd;

use super::room_protocol::TerminalSize;
use super::status_bar::{self, Bar};
use super::terminal_state::{RestoreGuard, child_output_enter_sequence, viewport_restore_sequence};

pub(crate) struct ViewportRenderer {
    stdout_fd: RawFd,
    // Drop-only sentinel: emits the restore sequence and uninstalls
    // signal handlers when the renderer is dropped. Never read after
    // construction — `set_fd` was removed to close a signal-handler
    // TOCTOU (see `INVARIANT_SIGNAL_RESTORE_FD_IS_STABLE` in
    // `terminal_state.rs`).
    _restore: RestoreGuard,
    parser: vt100::Parser,
    size: TerminalSize,
    previous_screen: Option<vt100::Screen>,
    previous_local_size: Option<TerminalSize>,
}

impl ViewportRenderer {
    pub(crate) fn enter(stdout_fd: RawFd, title: &str, bar: &Bar) -> anyhow::Result<Self> {
        let terminal = terminal_size(stdout_fd).unwrap_or(TerminalSize::new(80, 24));
        let size = remote_view_size(terminal);
        // Alt-screen enter must be IMMEDIATELY followed by cursor-home
        // (`\x1b[H`) — see `ALT_SCREEN_ENTER` and the
        // `alt_screen_enter_includes_explicit_cursor_home` /
        // `alt_screen_enter_homes_cursor` tests in `terminal_state.rs`.
        // Cursor-hide and screen-clear come AFTER the home, never
        // between the alt-screen enter and the home.
        write_all(stdout_fd, child_output_enter_sequence())?;
        write_all(stdout_fd, b"\x1b[?25l\x1b[2J")?;
        write_all(stdout_fd, &set_window_title_sequence(title))?;
        let mut renderer = Self {
            stdout_fd,
            _restore: RestoreGuard::new(stdout_fd, viewport_restore_sequence()),
            parser: vt100::Parser::new(size.rows, size.cols, 0),
            size,
            previous_screen: None,
            previous_local_size: None,
        };
        renderer.redraw_status(bar)?;
        Ok(renderer)
    }

    pub(crate) fn process_output(&mut self, bytes: &[u8], bar: &Bar) -> anyhow::Result<()> {
        self.parser.process(bytes);
        self.redraw(bar, false)
    }

    pub(crate) fn resize(
        &mut self,
        stdout_fd: RawFd,
        size: TerminalSize,
        bar: &Bar,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(
            stdout_fd, self.stdout_fd,
            "ViewportRenderer: stdout_fd must be stable for the renderer's \
             lifetime — see INVARIANT_SIGNAL_RESTORE_FD_IS_STABLE in \
             terminal_state.rs",
        );
        let mut force_full = false;
        if self.size != size {
            self.parser.screen_mut().set_size(size.rows, size.cols);
            self.size = size;
            force_full = true;
        }
        self.redraw(bar, force_full)
    }

    pub(crate) fn reported_size(stdout_fd: RawFd) -> Option<TerminalSize> {
        terminal_size(stdout_fd).map(remote_view_size)
    }

    pub(crate) fn redraw_status(&mut self, bar: &Bar) -> anyhow::Result<()> {
        let frame = status_bar::render(bar, terminal_size(self.stdout_fd));
        write_all(self.stdout_fd, &frame)
    }

    pub(crate) fn force_redraw(&mut self, stdout_fd: RawFd, bar: &Bar) -> anyhow::Result<()> {
        debug_assert_eq!(
            stdout_fd, self.stdout_fd,
            "ViewportRenderer: stdout_fd must be stable for the renderer's \
             lifetime — see INVARIANT_SIGNAL_RESTORE_FD_IS_STABLE in \
             terminal_state.rs",
        );
        self.redraw(bar, true)
    }

    fn redraw(&mut self, bar: &Bar, force_full: bool) -> anyhow::Result<()> {
        let terminal_size = terminal_size(self.stdout_fd);
        let local_size = terminal_size.map(remote_view_size);
        let mut frame = render_viewport(
            self.parser.screen(),
            self.previous_screen.as_ref(),
            local_size,
            self.previous_local_size,
            force_full,
        );
        self.previous_screen = Some(self.parser.screen().clone());
        self.previous_local_size = local_size;
        frame.extend_from_slice(&status_bar::render(bar, terminal_size));
        write_all(self.stdout_fd, &frame)
    }
}

fn render_viewport(
    screen: &vt100::Screen,
    previous_screen: Option<&vt100::Screen>,
    local_size: Option<TerminalSize>,
    previous_local_size: Option<TerminalSize>,
    force_full: bool,
) -> Vec<u8> {
    if should_render_full(
        screen,
        previous_screen,
        local_size,
        previous_local_size,
        force_full,
    ) {
        return render_viewport_full(screen, local_size);
    }

    let previous = previous_screen.expect("should_render_full requires previous screen");
    screen.state_diff(previous)
}

pub(crate) const fn remote_view_size(size: TerminalSize) -> TerminalSize {
    TerminalSize::new(size.cols, if size.rows > 1 { size.rows - 1 } else { 1 })
}

fn set_window_title_sequence(title: &str) -> Vec<u8> {
    let sanitized: String = title
        .chars()
        .filter(|ch| *ch != '\x07' && *ch != '\x1b')
        .collect();
    let mut out = Vec::with_capacity(sanitized.len() + 12);
    out.extend_from_slice(b"\x1b[22;2t");
    out.extend_from_slice(b"\x1b]2;");
    out.extend_from_slice(sanitized.as_bytes());
    out.push(b'\x07');
    out
}

fn should_render_full(
    screen: &vt100::Screen,
    previous_screen: Option<&vt100::Screen>,
    local_size: Option<TerminalSize>,
    previous_local_size: Option<TerminalSize>,
    force_full: bool,
) -> bool {
    let Some(previous) = previous_screen else {
        return true;
    };
    let (rows, cols) = screen.size();
    let local = local_size.unwrap_or(TerminalSize::new(cols, rows));
    force_full
        || previous.size() != screen.size()
        || previous_local_size != local_size
        || local.cols < cols
        || local.rows < rows
}

fn render_viewport_full(screen: &vt100::Screen, local_size: Option<TerminalSize>) -> Vec<u8> {
    let (rows, cols) = screen.size();
    let local = local_size.unwrap_or(TerminalSize::new(cols, rows));
    let rows = rows.min(local.rows);
    let cols = cols.min(local.cols);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[H\x1b[2J");
    for (idx, row) in screen.rows_formatted(0, cols).enumerate() {
        let Ok(row_num) = u16::try_from(idx + 1) else {
            break;
        };
        if row_num > rows {
            break;
        }
        out.extend_from_slice(format!("\x1b[{row_num};1H").as_bytes());
        out.extend_from_slice(&row);
    }
    out.extend_from_slice(&screen.input_mode_formatted());
    out.extend_from_slice(&screen.cursor_state_formatted());
    out
}

use super::terminal_io::{terminal_size, write_all};

#[cfg(test)]
mod tests {
    use super::{
        remote_view_size, render_viewport, render_viewport_full, set_window_title_sequence,
    };
    use crate::pty::room_protocol::TerminalSize;

    #[test]
    fn set_window_title_pushes_then_sets() {
        let bytes = set_window_title_sequence("ptyroom host 127.0.0.1:7373");
        let text = String::from_utf8_lossy(&bytes);

        assert!(text.starts_with("\x1b[22;2t"));
        assert!(text.contains("\x1b]2;ptyroom host 127.0.0.1:7373\x07"));
        assert!(bytes.ends_with(b"\x07"));
    }

    #[test]
    fn set_window_title_strips_control_chars() {
        let bytes = set_window_title_sequence("evil\x07title\x1bhere");
        let text = String::from_utf8_lossy(&bytes);

        assert!(text.contains("eviltitlehere"));
    }

    #[test]
    fn viewport_renderer_clips_to_local_terminal_size() {
        let mut parser = vt100::Parser::new(2, 5, 0);
        parser.process(b"hello\r\nworld");

        let rendered =
            render_viewport_full(parser.screen(), Some(TerminalSize { cols: 3, rows: 1 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("hel"));
        assert!(!text.contains("world"));
    }

    #[test]
    fn remote_view_size_reserves_one_status_row() {
        assert_eq!(
            remote_view_size(TerminalSize { cols: 80, rows: 24 }),
            TerminalSize { cols: 80, rows: 23 }
        );
        assert_eq!(
            remote_view_size(TerminalSize { cols: 80, rows: 1 }),
            TerminalSize { cols: 80, rows: 1 }
        );
    }

    #[test]
    fn viewport_renderer_uses_diff_without_clearing_when_size_is_stable() {
        let mut previous = vt100::Parser::new(2, 8, 0);
        previous.process(b"hello");
        let mut parser = vt100::Parser::new(2, 8, 0);
        parser.process(b"hello!");
        let current = parser.screen().clone();

        let rendered = render_viewport(
            &current,
            Some(previous.screen()),
            Some(TerminalSize { cols: 8, rows: 2 }),
            Some(TerminalSize { cols: 8, rows: 2 }),
            false,
        );

        assert!(!contains_bytes(&rendered, b"\x1b[2J"));
        assert!(contains_bytes(&rendered, b"!"));
    }

    #[test]
    fn viewport_renderer_force_redraw_uses_full_render_even_when_size_is_stable() {
        let mut previous = vt100::Parser::new(2, 8, 0);
        previous.process(b"hello");
        let mut current = vt100::Parser::new(2, 8, 0);
        current.process(b"hello!");

        let rendered = render_viewport(
            current.screen(),
            Some(previous.screen()),
            Some(TerminalSize { cols: 8, rows: 2 }),
            Some(TerminalSize { cols: 8, rows: 2 }),
            true,
        );

        assert!(contains_bytes(&rendered, b"\x1b[2J"));
        assert!(String::from_utf8_lossy(&rendered).contains("hello!"));
    }

    #[test]
    fn viewport_renderer_clears_when_local_size_changes() {
        let mut previous = vt100::Parser::new(2, 8, 0);
        previous.process(b"hello");
        let mut current = vt100::Parser::new(2, 8, 0);
        current.process(b"hello!");

        let rendered = render_viewport(
            current.screen(),
            Some(previous.screen()),
            Some(TerminalSize { cols: 10, rows: 4 }),
            Some(TerminalSize { cols: 8, rows: 2 }),
            false,
        );

        assert!(contains_bytes(&rendered, b"\x1b[2J"));
    }

    #[test]
    fn viewport_renderer_clears_when_screen_exceeds_local_size() {
        let mut previous = vt100::Parser::new(2, 8, 0);
        previous.process(b"hello");
        let mut current = vt100::Parser::new(2, 8, 0);
        current.process(b"hello!");

        let rendered = render_viewport(
            current.screen(),
            Some(previous.screen()),
            Some(TerminalSize { cols: 4, rows: 1 }),
            Some(TerminalSize { cols: 4, rows: 1 }),
            false,
        );

        assert!(contains_bytes(&rendered, b"\x1b[2J"));
        assert!(!String::from_utf8_lossy(&rendered).contains("hello!"));
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
