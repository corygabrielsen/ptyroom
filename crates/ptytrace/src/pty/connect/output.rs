//! Local output rendering for `ptyroom join` and `ptyroom watch`.

use std::os::fd::RawFd;

use super::super::room_protocol::TerminalSize;
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

fn render_status_line(
    terminal_size: Option<TerminalSize>,
    status: LocalStatus,
    room_label: &str,
    local_controls: bool,
    read_only: bool,
) -> Vec<u8> {
    let Some(size) = terminal_size else {
        return Vec::new();
    };
    let row = size.rows;
    let mut text = match status {
        LocalStatus::Connected if local_controls && read_only => {
            format!(" ptyroom watch {room_label} | read-only | {LOCAL_ESCAPE_NAME} ? help")
        }
        LocalStatus::Connected if local_controls => {
            format!(" ptyroom {room_label} | {LOCAL_ESCAPE_NAME} ? help")
        }
        LocalStatus::Connected if read_only => format!(" ptyroom watch {room_label} | read-only"),
        LocalStatus::Connected => format!(" ptyroom join {room_label}"),
        LocalStatus::Command => {
            if read_only {
                " ptyroom watch command | . detach | ? help | r redraw".to_owned()
            } else {
                format!(
                    " ptyroom join command | . detach | ? help | r redraw | {LOCAL_ESCAPE_NAME} send"
                )
            }
        }
        LocalStatus::Help if read_only => format!(
            " ptyroom watch controls | {LOCAL_ESCAPE_NAME} . detach | {LOCAL_ESCAPE_NAME} r redraw | read-only"
        ),
        LocalStatus::Help => format!(
            " ptyroom join controls | {LOCAL_ESCAPE_NAME} . detach | {LOCAL_ESCAPE_NAME} r redraw | {LOCAL_ESCAPE_NAME} {LOCAL_ESCAPE_NAME} send {LOCAL_ESCAPE_NAME}"
        ),
    };
    let max_len = usize::from(size.cols.saturating_sub(1));
    if text.len() > max_len {
        text.truncate(max_len);
    }

    let mut out = Vec::new();
    out.extend_from_slice(format!("\x1b7\x1b[{row};1H\x1b[7m\x1b[2K").as_bytes());
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[0m\x1b8");
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

#[cfg(test)]
mod tests {
    use super::{
        LocalStatus, remote_view_size, render_status_line, render_viewport, render_viewport_full,
    };
    use crate::pty::room_protocol::TerminalSize;

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
            Some(TerminalSize { cols: 40, rows: 5 }),
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
    fn status_line_preserves_cursor_and_visual_state() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 40, rows: 5 }),
            LocalStatus::Connected,
            "room",
            true,
            false,
        );

        assert!(rendered.starts_with(b"\x1b7\x1b[5;1H\x1b[7m\x1b[2K"));
        assert!(rendered.ends_with(b"\x1b[0m\x1b8"));
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

        assert!(text.contains("ptyroom join 127.0.0.1:7373"));
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

        assert!(text.contains("ptyroom watch 127.0.0.1:7373"));
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
        let text = String::from_utf8_lossy(&rendered);
        let visible = status_visible_text(&text);

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

    fn status_visible_text(rendered: &str) -> &str {
        let start = rendered.find("\x1b[2K").unwrap() + "\x1b[2K".len();
        let end = rendered[start..].find("\x1b[0m").unwrap() + start;
        &rendered[start..end]
    }
}
