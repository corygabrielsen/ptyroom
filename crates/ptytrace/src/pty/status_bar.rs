//! Composable status-bar rendering for ptyroom viewports.
//!
//! A [`Bar`] is a chip (mode identity) plus an ordered list of body
//! segments. [`render`] paints it on the bottom row of the terminal as a
//! single reverse-video line with the chip rendered as a colored block at
//! the left edge. Callers build their own `Bar` for each frame; this
//! module owns layout and ANSI emission, not mode-specific text.

use super::room_protocol::TerminalSize;

const RESET: &[u8] = b"\x1b[0m";
const REVERSE_ON: &[u8] = b"\x1b[7m";
const SAVE_CURSOR: &[u8] = b"\x1b7";
const RESTORE_CURSOR: &[u8] = b"\x1b8";
const CLEAR_LINE: &[u8] = b"\x1b[2K";
const SEPARATOR: &str = " | ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chip {
    Host,
    Join,
    Watch,
}

impl Chip {
    const fn fg_sgr(self) -> &'static [u8] {
        match self {
            Self::Host => b"\x1b[1;32m",
            Self::Join => b"\x1b[1;36m",
            Self::Watch => b"\x1b[1;33m",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Host => "HOST",
            Self::Join => "JOIN",
            Self::Watch => "WATCH",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Bar {
    pub chip: Chip,
    pub segments: Vec<String>,
}

impl Bar {
    pub(crate) fn new(chip: Chip) -> Self {
        Self {
            chip,
            segments: Vec::new(),
        }
    }

    pub(crate) fn segment(mut self, text: impl Into<String>) -> Self {
        self.segments.push(text.into());
        self
    }
}

pub(crate) fn render(bar: &Bar, terminal_size: Option<TerminalSize>) -> Vec<u8> {
    let Some(size) = terminal_size else {
        return Vec::new();
    };
    let row = size.rows;
    let max_visible = usize::from(size.cols.saturating_sub(1));
    if max_visible == 0 {
        return Vec::new();
    }

    let chip_label = bar.chip.label();
    let chip_visible = chip_label.len() + 2;
    let body = bar.segments.join(SEPARATOR);

    let mut out = Vec::new();
    out.extend_from_slice(SAVE_CURSOR);
    out.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
    out.extend_from_slice(CLEAR_LINE);

    out.extend_from_slice(bar.chip.fg_sgr());
    out.extend_from_slice(REVERSE_ON);
    out.extend_from_slice(format!(" {chip_label} ").as_bytes());
    out.extend_from_slice(RESET);

    let body_room = max_visible.saturating_sub(chip_visible);
    if body_room > 0 && !body.is_empty() {
        out.extend_from_slice(REVERSE_ON);
        out.push(b' ');
        let body_room_after_pad = body_room.saturating_sub(1);
        let body_visible = body.chars().count();
        let truncated: String = if body_visible <= body_room_after_pad {
            body
        } else {
            body.chars().take(body_room_after_pad).collect()
        };
        let truncated_len = truncated.chars().count();
        out.extend_from_slice(truncated.as_bytes());
        let used = 1 + truncated_len;
        out.extend(std::iter::repeat_n(b' ', body_room.saturating_sub(used)));
        out.extend_from_slice(RESET);
    } else if body_room > 0 {
        out.extend_from_slice(REVERSE_ON);
        out.extend(std::iter::repeat_n(b' ', body_room));
        out.extend_from_slice(RESET);
    }

    out.extend_from_slice(RESTORE_CURSOR);
    out
}

#[cfg(test)]
mod tests {
    use super::{Bar, Chip, render};
    use crate::pty::room_protocol::TerminalSize;

    fn visible_text(rendered: &[u8]) -> String {
        let text = String::from_utf8_lossy(rendered).to_string();
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

    #[test]
    fn render_without_terminal_size_is_empty() {
        let bar = Bar::new(Chip::Join).segment("127.0.0.1:7373");
        assert!(render(&bar, None).is_empty());
    }

    #[test]
    fn render_places_status_on_bottom_row() {
        let bar = Bar::new(Chip::Join).segment("127.0.0.1:7373");
        let rendered = render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("\x1b[24;1H"));
        assert!(text.starts_with("\x1b7"));
        assert!(text.ends_with("\x1b8"));
    }

    #[test]
    fn render_emits_chip_label_with_mode_color() {
        let bar = Bar::new(Chip::Watch).segment("127.0.0.1:7373");
        let rendered = render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = String::from_utf8_lossy(&rendered);

        assert!(text.contains("\x1b[1;33m"));
        assert!(text.contains(" WATCH "));
    }

    #[test]
    fn render_segments_join_with_separator() {
        let bar = Bar::new(Chip::Join)
            .segment("127.0.0.1:7373")
            .segment("^] ? help");
        let rendered = render(&bar, Some(TerminalSize { cols: 80, rows: 24 }));
        let text = visible_text(&rendered);

        assert!(text.contains("127.0.0.1:7373 | ^] ? help"));
    }

    #[test]
    fn render_truncates_long_body_to_terminal_width() {
        let bar = Bar::new(Chip::Watch).segment("x".repeat(200));
        let rendered = render(&bar, Some(TerminalSize { cols: 20, rows: 5 }));
        let text = visible_text(&rendered);

        assert!(text.len() <= 19, "rendered visible text was {text:?}");
        assert!(text.contains("WATCH"));
    }

    #[test]
    fn render_pads_body_to_fill_bar_width() {
        let bar = Bar::new(Chip::Join).segment("hi");
        let rendered = render(&bar, Some(TerminalSize { cols: 20, rows: 5 }));
        let text = visible_text(&rendered);

        assert_eq!(text.len(), 19);
        assert!(text.contains("JOIN"));
        assert!(text.contains("hi"));
    }
}
