//! Local output rendering for `ptyroom join` and `ptyroom watch`.

use std::os::fd::RawFd;

use super::super::room_protocol::TerminalSize;
use super::super::status_bar::{self, Bar, Chip};
use super::super::terminal_state::{RestoreGuard, viewport_restore_sequence};
use super::control::{LOCAL_ESCAPE_NAME, LocalStatus};
use super::terminal::{terminal_size, write_all};

pub(super) enum OutputSink {
    Raw,
    Viewport(Box<ViewportRenderer>),
}

impl OutputSink {
    pub(super) fn viewport(
        stdout_fd: RawFd,
        room_label: String,
        local_controls: bool,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        Ok(Self::Viewport(Box::new(ViewportRenderer::enter(
            stdout_fd,
            room_label,
            local_controls,
            read_only,
        )?)))
    }

    pub(super) fn write_output(&mut self, stdout_fd: RawFd, bytes: &[u8]) -> anyhow::Result<()> {
        match self {
            Self::Raw => write_all(stdout_fd, bytes),
            Self::Viewport(renderer) => renderer.process_output(bytes),
        }
    }

    pub(super) fn resize(&mut self, stdout_fd: RawFd, size: TerminalSize) -> anyhow::Result<()> {
        match self {
            Self::Raw => Ok(()),
            Self::Viewport(renderer) => renderer.resize(stdout_fd, size),
        }
    }

    pub(super) fn reports_size(&self) -> bool {
        matches!(self, Self::Viewport(_))
    }

    pub(super) fn reported_size(&self, stdout_fd: RawFd) -> Option<TerminalSize> {
        match self {
            Self::Raw => terminal_size(stdout_fd),
            Self::Viewport(_) => ViewportRenderer::reported_size(stdout_fd),
        }
    }

    pub(super) fn set_status(
        &mut self,
        stdout_fd: RawFd,
        status: LocalStatus,
    ) -> anyhow::Result<()> {
        match self {
            Self::Raw => Ok(()),
            Self::Viewport(renderer) => renderer.set_status(stdout_fd, status),
        }
    }

    pub(super) fn force_redraw(&mut self, stdout_fd: RawFd) -> anyhow::Result<()> {
        match self {
            Self::Raw => Ok(()),
            Self::Viewport(renderer) => renderer.force_redraw(stdout_fd),
        }
    }
}

pub(super) struct ViewportRenderer {
    stdout_fd: RawFd,
    restore: RestoreGuard,
    room_label: String,
    local_controls: bool,
    read_only: bool,
    status: LocalStatus,
    parser: vt100::Parser,
    size: TerminalSize,
    previous_screen: Option<vt100::Screen>,
    previous_local_size: Option<TerminalSize>,
}

impl ViewportRenderer {
    fn enter(
        stdout_fd: RawFd,
        room_label: String,
        local_controls: bool,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        let terminal = terminal_size(stdout_fd).unwrap_or(TerminalSize::new(80, 24));
        let size = remote_view_size(terminal);
        write_all(stdout_fd, b"\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J")?;
        let title_mode = if read_only { "watch" } else { "join" };
        let title = format!("ptyroom {title_mode} {room_label}");
        write_all(stdout_fd, &set_window_title_sequence(&title))?;
        let mut renderer = Self {
            stdout_fd,
            restore: RestoreGuard::new(stdout_fd, viewport_restore_sequence()),
            room_label,
            local_controls,
            read_only,
            status: LocalStatus::Connected,
            parser: vt100::Parser::new(size.rows, size.cols, 0),
            size,
            previous_screen: None,
            previous_local_size: None,
        };
        renderer.redraw_status()?;
        Ok(renderer)
    }

    fn process_output(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.parser.process(bytes);
        self.redraw(false)
    }

    fn resize(&mut self, stdout_fd: RawFd, size: TerminalSize) -> anyhow::Result<()> {
        let mut force_full = false;
        if self.size != size {
            self.parser.screen_mut().set_size(size.rows, size.cols);
            self.size = size;
            force_full = true;
        }
        self.stdout_fd = stdout_fd;
        self.restore.set_fd(stdout_fd);
        self.redraw(force_full)
    }

    fn reported_size(stdout_fd: RawFd) -> Option<TerminalSize> {
        terminal_size(stdout_fd).map(remote_view_size)
    }

    fn set_status(&mut self, stdout_fd: RawFd, status: LocalStatus) -> anyhow::Result<()> {
        self.stdout_fd = stdout_fd;
        self.restore.set_fd(stdout_fd);
        self.status = status;
        self.redraw_status()
    }

    fn force_redraw(&mut self, stdout_fd: RawFd) -> anyhow::Result<()> {
        self.stdout_fd = stdout_fd;
        self.restore.set_fd(stdout_fd);
        self.redraw(true)
    }

    fn redraw(&mut self, force_full: bool) -> anyhow::Result<()> {
        let terminal_size = terminal_size(self.stdout_fd);
        let local_size = terminal_size.map(remote_view_size);
        let frame = render_viewport(
            self.parser.screen(),
            self.previous_screen.as_ref(),
            local_size,
            self.previous_local_size,
            force_full,
        );
        self.previous_screen = Some(self.parser.screen().clone());
        self.previous_local_size = local_size;
        let mut frame = frame;
        frame.extend_from_slice(&render_status_line(
            terminal_size,
            self.status,
            &self.room_label,
            self.local_controls,
            self.read_only,
        ));
        write_all(self.stdout_fd, &frame)
    }

    fn redraw_status(&mut self) -> anyhow::Result<()> {
        let frame = render_status_line(
            terminal_size(self.stdout_fd),
            self.status,
            &self.room_label,
            self.local_controls,
            self.read_only,
        );
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

const fn remote_view_size(size: TerminalSize) -> TerminalSize {
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

fn render_status_line(
    terminal_size: Option<TerminalSize>,
    status: LocalStatus,
    room_label: &str,
    local_controls: bool,
    read_only: bool,
) -> Vec<u8> {
    let chip = if read_only { Chip::Watch } else { Chip::Join };
    let mut bar = Bar::new(chip).segment(room_label);
    if read_only {
        bar = bar.segment("read-only");
    }
    match status {
        LocalStatus::Connected => {
            if local_controls {
                bar = bar.segment(format!("{LOCAL_ESCAPE_NAME} ? help"));
            }
        }
        LocalStatus::Command => {
            bar = bar
                .segment("command")
                .segment(". detach")
                .segment("? help")
                .segment("r redraw");
            if !read_only {
                bar = bar.segment(format!("{LOCAL_ESCAPE_NAME} send"));
            }
        }
        LocalStatus::Help => {
            bar = bar
                .segment("controls")
                .segment(format!("{LOCAL_ESCAPE_NAME} . detach"))
                .segment(format!("{LOCAL_ESCAPE_NAME} r redraw"));
            if !read_only {
                bar = bar.segment(format!(
                    "{LOCAL_ESCAPE_NAME} {LOCAL_ESCAPE_NAME} send {LOCAL_ESCAPE_NAME}"
                ));
            }
        }
    }
    status_bar::render(&bar, terminal_size)
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

#[cfg(test)]
mod tests {
    use super::{
        LocalStatus, remote_view_size, render_status_line, render_viewport, render_viewport_full,
        set_window_title_sequence,
    };
    use crate::pty::room_protocol::TerminalSize;

    #[test]
    fn set_window_title_pushes_then_sets() {
        let bytes = set_window_title_sequence("ptyroom join 127.0.0.1:7373");
        let text = String::from_utf8_lossy(&bytes);

        assert!(text.starts_with("\x1b[22;2t"));
        assert!(text.contains("\x1b]2;ptyroom join 127.0.0.1:7373\x07"));
        assert!(bytes.ends_with(b"\x07"));
    }

    #[test]
    fn set_window_title_strips_control_chars() {
        let bytes = set_window_title_sequence("evil\x07title\x1bhere");
        let text = String::from_utf8_lossy(&bytes);

        assert!(text.contains("eviltitlehere"));
        let body_start = text.find("\x1b]2;").unwrap() + "\x1b]2;".len();
        let body_end = text.rfind('\x07').unwrap();
        let body = &text[body_start..body_end];
        assert!(!body.contains('\x07'));
        assert!(!body.contains('\x1b'));
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
    fn status_line_renders_on_bottom_row_without_newline() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 80, rows: 5 }),
            LocalStatus::Command,
            "127.0.0.1:7373",
            true,
            false,
        );
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("\x1b[5;1H"));
        assert!(text.contains(". detach"));
        assert!(!text.contains('\n'));
    }

    #[test]
    fn status_line_preserves_cursor_and_clears_row() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 40, rows: 5 }),
            LocalStatus::Connected,
            "room",
            true,
            false,
        );

        assert!(rendered.starts_with(b"\x1b7"));
        assert!(rendered.ends_with(b"\x1b8"));
        assert!(contains_bytes(&rendered, b"\x1b[5;1H"));
        assert!(contains_bytes(&rendered, b"\x1b[2K"));
    }

    #[test]
    fn join_chip_uses_join_color() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 80, rows: 24 }),
            LocalStatus::Connected,
            "127.0.0.1:7373",
            true,
            false,
        );
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains(" JOIN "));
        assert!(text.contains("\x1b[1;36m"));
        assert!(text.contains("127.0.0.1:7373"));
        assert!(text.contains("^] ? help"));
    }

    #[test]
    fn connected_status_omits_local_help_when_controls_are_disabled() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 80, rows: 24 }),
            LocalStatus::Connected,
            "127.0.0.1:7373",
            false,
            false,
        );
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains(" JOIN "));
        assert!(text.contains("127.0.0.1:7373"));
        assert!(!text.contains("^]"));
        assert!(!text.contains("help"));
    }

    #[test]
    fn read_only_status_identifies_watch_mode() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 80, rows: 24 }),
            LocalStatus::Connected,
            "127.0.0.1:7373",
            true,
            true,
        );
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains(" WATCH "));
        assert!(text.contains("\x1b[1;33m"));
        assert!(text.contains("127.0.0.1:7373"));
        assert!(text.contains("read-only"));
        assert!(text.contains("^] ? help"));
    }

    #[test]
    fn status_line_truncates_to_terminal_width() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 12, rows: 3 }),
            LocalStatus::Help,
            "127.0.0.1:7373",
            true,
            false,
        );
        let visible = status_visible_text(&rendered);

        assert!(visible.len() <= 11, "visible status text was {visible:?}");
    }

    #[test]
    fn status_line_without_terminal_size_is_empty() {
        assert!(render_status_line(None, LocalStatus::Connected, "room", true, false).is_empty());
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

    fn status_visible_text(rendered: &[u8]) -> String {
        let text = String::from_utf8_lossy(rendered);
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for inner in chars.by_ref() {
                    if inner.is_ascii_alphabetic() || inner == '\\' || inner == '\x07' {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }
}
