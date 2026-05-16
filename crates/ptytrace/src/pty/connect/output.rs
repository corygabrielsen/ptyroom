//! Local output rendering for `ptyroom join` and `ptyroom watch`.

use std::os::fd::RawFd;

use super::super::room_protocol::TerminalSize;
use super::super::status_bar::{Bar, Chip};
use super::super::terminal_io::{terminal_size, write_all};
use super::super::viewport::ViewportRenderer;
use super::control::{LOCAL_ESCAPE_NAME, LocalStatus};

pub(super) enum OutputSink {
    Raw,
    Viewport(Box<ConnectViewport>),
}

impl OutputSink {
    pub(super) fn viewport(
        stdout_fd: RawFd,
        room_label: String,
        local_controls: bool,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        Ok(Self::Viewport(Box::new(ConnectViewport::enter(
            stdout_fd,
            room_label,
            local_controls,
            read_only,
        )?)))
    }

    pub(super) fn write_output(&mut self, stdout_fd: RawFd, bytes: &[u8]) -> anyhow::Result<()> {
        match self {
            Self::Raw => write_all(stdout_fd, bytes),
            Self::Viewport(view) => view.process_output(bytes),
        }
    }

    pub(super) fn resize(&mut self, stdout_fd: RawFd, size: TerminalSize) -> anyhow::Result<()> {
        match self {
            Self::Raw => Ok(()),
            Self::Viewport(view) => view.resize(stdout_fd, size),
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
            Self::Viewport(view) => view.set_status(stdout_fd, status),
        }
    }

    pub(super) fn force_redraw(&mut self, stdout_fd: RawFd) -> anyhow::Result<()> {
        match self {
            Self::Raw => Ok(()),
            Self::Viewport(view) => view.force_redraw(stdout_fd),
        }
    }
}

pub(super) struct ConnectViewport {
    renderer: ViewportRenderer,
    room_label: String,
    local_controls: bool,
    read_only: bool,
    status: LocalStatus,
}

impl ConnectViewport {
    fn enter(
        stdout_fd: RawFd,
        room_label: String,
        local_controls: bool,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        let title_mode = if read_only { "watch" } else { "join" };
        let title = format!("ptyroom {title_mode} {room_label}");
        let bar = build_bar(
            &room_label,
            LocalStatus::Connected,
            local_controls,
            read_only,
        );
        let renderer = ViewportRenderer::enter(stdout_fd, &title, &bar)?;
        Ok(Self {
            renderer,
            room_label,
            local_controls,
            read_only,
            status: LocalStatus::Connected,
        })
    }

    fn process_output(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.renderer.process_output(bytes, &self.bar())
    }

    fn resize(&mut self, stdout_fd: RawFd, size: TerminalSize) -> anyhow::Result<()> {
        self.renderer.resize(stdout_fd, size, &self.bar())
    }

    fn set_status(&mut self, _stdout_fd: RawFd, status: LocalStatus) -> anyhow::Result<()> {
        self.status = status;
        self.renderer.redraw_status(&self.bar())
    }

    fn force_redraw(&mut self, stdout_fd: RawFd) -> anyhow::Result<()> {
        self.renderer.force_redraw(stdout_fd, &self.bar())
    }

    fn bar(&self) -> Bar {
        build_bar(
            &self.room_label,
            self.status,
            self.local_controls,
            self.read_only,
        )
    }
}

fn build_bar(room_label: &str, status: LocalStatus, local_controls: bool, read_only: bool) -> Bar {
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
    bar
}

#[cfg(test)]
mod tests {
    use super::super::super::status_bar;
    use super::{LocalStatus, build_bar};
    use crate::pty::room_protocol::TerminalSize;

    fn render_status_line(
        terminal_size: Option<TerminalSize>,
        status: LocalStatus,
        room_label: &str,
        local_controls: bool,
        read_only: bool,
    ) -> Vec<u8> {
        let bar = build_bar(room_label, status, local_controls, read_only);
        status_bar::render(&bar, terminal_size)
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
    fn watch_command_state_drops_send_segment() {
        let rendered = render_status_line(
            Some(TerminalSize { cols: 80, rows: 24 }),
            LocalStatus::Command,
            "127.0.0.1:7373",
            true,
            true,
        );
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains(" WATCH "));
        assert!(text.contains(". detach"));
        assert!(!text.contains("^] send"));
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
