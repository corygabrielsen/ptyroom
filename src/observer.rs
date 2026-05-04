//! Deterministic observers and semantic predicates.
//!
//! An observer is the smallest state machine that can replay raw output bytes
//! and answer predicates about the resulting state. This module starts with a
//! synthetic text observer so the proof pipeline can be tested without binding
//! the architecture to any specific terminal implementation.

use serde::{Deserialize, Serialize};

use crate::proof::StateHash;

/// Semantic fact extracted from an observed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Fact {
    TextBytes { count: usize },
    EventCount { count: u64 },
    StateHash { hash: StateHash },
}

/// Snapshot of an observer state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedState {
    text: String,
    event_count: u64,
    hash: StateHash,
}

impl ObservedState {
    #[must_use]
    pub fn new(text: String, event_count: u64) -> Self {
        let hash = stable_state_hash(text.as_bytes(), event_count);
        Self {
            text,
            event_count,
            hash,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    #[must_use]
    pub const fn hash(&self) -> StateHash {
        self.hash
    }

    #[must_use]
    pub fn facts(&self) -> Vec<Fact> {
        vec![
            Fact::TextBytes {
                count: self.text.len(),
            },
            Fact::EventCount {
                count: self.event_count,
            },
            Fact::StateHash { hash: self.hash },
        ]
    }
}

/// Predicate that can be checked against an [`ObservedState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Predicate {
    ContainsText { text: String },
    DoesNotContainText { text: String },
    StateEquals { hash: StateHash },
    EventCountIs { count: u64 },
}

impl Predicate {
    #[must_use]
    pub fn matches(&self, state: &ObservedState) -> bool {
        match self {
            Self::ContainsText { text } => state.text.contains(text),
            Self::DoesNotContainText { text } => !state.text.contains(text),
            Self::StateEquals { hash } => state.hash == *hash,
            Self::EventCountIs { count } => state.event_count == *count,
        }
    }
}

/// Deterministic state machine over output bytes.
pub trait Observer {
    fn apply_output(&mut self, bytes: &[u8]);
    fn state(&self) -> ObservedState;

    #[must_use]
    fn satisfies(&self, predicate: &Predicate) -> bool {
        predicate.matches(&self.state())
    }
}

/// Minimal observer for architecture tests and synthetic fixtures.
#[derive(Debug, Clone, Default)]
pub struct SyntheticObserver {
    text: String,
    event_count: u64,
}

impl SyntheticObserver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Observer for SyntheticObserver {
    fn apply_output(&mut self, bytes: &[u8]) {
        self.text.push_str(&String::from_utf8_lossy(bytes));
        self.event_count = self.event_count.saturating_add(1);
    }

    fn state(&self) -> ObservedState {
        ObservedState::new(self.text.clone(), self.event_count)
    }
}

/// Small deterministic terminal-like screen observer.
///
/// This is not a full VT implementation. It exists to verify recorder traces
/// against visible text and cursor/clear behavior without launching the
/// TypeScript snapshot renderer. Unknown control sequences are skipped.
#[derive(Debug, Clone)]
pub struct ScreenObserver {
    cols: usize,
    rows: usize,
    cursor_x: usize,
    cursor_y: usize,
    cells: Vec<char>,
    event_count: u64,
}

impl ScreenObserver {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = usize::from(cols.max(1));
        let rows = usize::from(rows.max(1));
        Self {
            cols,
            rows,
            cursor_x: 0,
            cursor_y: 0,
            cells: vec![' '; cols * rows],
            event_count: 0,
        }
    }

    #[must_use]
    pub fn screen_text(&self) -> String {
        let mut lines = Vec::with_capacity(self.rows);
        for y in 0..self.rows {
            let start = y * self.cols;
            let end = start + self.cols;
            let mut line: String = self.cells[start..end].iter().collect();
            line.truncate(line.trim_end().len());
            lines.push(line);
        }
        let mut text = lines.join("\n");
        text.truncate(text.trim_end().len());
        text
    }

    fn put_char(&mut self, ch: char) {
        if self.cursor_x >= self.cols {
            self.cursor_x = 0;
            self.line_feed();
        }
        let idx = self.cursor_y * self.cols + self.cursor_x;
        self.cells[idx] = ch;
        self.cursor_x += 1;
    }

    fn carriage_return(&mut self) {
        self.cursor_x = 0;
    }

    fn line_feed(&mut self) {
        if self.cursor_y + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cursor_y += 1;
        }
    }

    fn scroll_up(&mut self) {
        self.cells.copy_within(self.cols.., 0);
        let start = (self.rows - 1) * self.cols;
        self.cells[start..].fill(' ');
    }

    fn clear_screen(&mut self) {
        self.cells.fill(' ');
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    fn clear_line_from_cursor(&mut self) {
        let start = self.cursor_y * self.cols + self.cursor_x;
        let end = (self.cursor_y + 1) * self.cols;
        self.cells[start..end].fill(' ');
    }

    fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_y = row.saturating_sub(1).min(self.rows - 1);
        self.cursor_x = col.saturating_sub(1).min(self.cols - 1);
    }

    fn move_cursor(&mut self, dx: isize, dy: isize) {
        self.cursor_x = self.cursor_x.saturating_add_signed(dx).min(self.cols - 1);
        self.cursor_y = self.cursor_y.saturating_add_signed(dy).min(self.rows - 1);
    }

    fn apply_csi(&mut self, params: &[u16], final_byte: u8) {
        let amount =
            isize::try_from(params.first().copied().unwrap_or(1).max(1)).unwrap_or(isize::MAX);
        match final_byte {
            b'A' => self.move_cursor(0, -amount),
            b'B' => self.move_cursor(0, amount),
            b'C' => self.move_cursor(amount, 0),
            b'D' => self.move_cursor(-amount, 0),
            b'H' | b'f' => {
                let row = usize::from(params.first().copied().unwrap_or(1));
                let col = usize::from(params.get(1).copied().unwrap_or(1));
                self.set_cursor(row, col);
            }
            b'J' => self.clear_screen(),
            b'K' => self.clear_line_from_cursor(),
            _ => {}
        }
    }
}

impl Observer for ScreenObserver {
    fn apply_output(&mut self, bytes: &[u8]) {
        self.event_count = self.event_count.saturating_add(1);
        let mut idx = 0;
        while idx < bytes.len() {
            match bytes[idx] {
                b'\r' => {
                    self.carriage_return();
                    idx += 1;
                }
                b'\n' => {
                    self.line_feed();
                    idx += 1;
                }
                0x1b => idx = self.consume_escape(bytes, idx + 1),
                0x20..=0x7e => {
                    self.put_char(char::from(bytes[idx]));
                    idx += 1;
                }
                _ => idx += 1,
            }
        }
    }

    fn state(&self) -> ObservedState {
        ObservedState::new(self.screen_text(), self.event_count)
    }
}

impl ScreenObserver {
    fn consume_escape(&mut self, bytes: &[u8], idx: usize) -> usize {
        let Some(&kind) = bytes.get(idx) else {
            return idx;
        };
        match kind {
            b'[' => self.consume_csi(bytes, idx + 1),
            b']' => consume_osc(bytes, idx + 1),
            _ => idx + 1,
        }
    }

    fn consume_csi(&mut self, bytes: &[u8], mut idx: usize) -> usize {
        let params_start = idx;
        while let Some(&byte) = bytes.get(idx) {
            if (0x40..=0x7e).contains(&byte) {
                let params = parse_csi_params(&bytes[params_start..idx]);
                self.apply_csi(&params, byte);
                return idx + 1;
            }
            idx += 1;
        }
        idx
    }
}

fn consume_osc(bytes: &[u8], mut idx: usize) -> usize {
    while let Some(&byte) = bytes.get(idx) {
        if byte == 0x07 {
            return idx + 1;
        }
        if byte == 0x1b && bytes.get(idx + 1) == Some(&b'\\') {
            return idx + 2;
        }
        idx += 1;
    }
    idx
}

fn parse_csi_params(bytes: &[u8]) -> Vec<u16> {
    String::from_utf8_lossy(bytes)
        .split(';')
        .filter_map(parse_csi_param)
        .collect()
}

fn parse_csi_param(param: &str) -> Option<u16> {
    let digits: String = param.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[must_use]
fn stable_state_hash(text: &[u8], event_count: u64) -> StateHash {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.iter().chain(event_count.to_le_bytes().iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    StateHash::new(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_observer_accumulates_lossy_text() {
        let mut observer = SyntheticObserver::new();
        observer.apply_output(b"hello ");
        observer.apply_output(&[b'w', b'o', b'r', b'l', b'd', 0xff]);

        let state = observer.state();
        assert!(state.text().contains("hello world"));
        assert_eq!(state.event_count(), 2);
    }

    #[test]
    fn predicates_match_observed_state() {
        let state = ObservedState::new("alpha beta".into(), 3);
        assert!(
            Predicate::ContainsText {
                text: "beta".into()
            }
            .matches(&state)
        );
        assert!(
            Predicate::DoesNotContainText {
                text: "gamma".into()
            }
            .matches(&state)
        );
        assert!(Predicate::EventCountIs { count: 3 }.matches(&state));
        assert!(Predicate::StateEquals { hash: state.hash() }.matches(&state));
    }

    #[test]
    fn state_hash_is_stable_and_state_sensitive() {
        let a = ObservedState::new("same".into(), 1).hash();
        let b = ObservedState::new("same".into(), 1).hash();
        let c = ObservedState::new("same".into(), 2).hash();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn facts_expose_state_summary() {
        let state = ObservedState::new("abc".into(), 2);
        assert_eq!(
            state.facts(),
            vec![
                Fact::TextBytes { count: 3 },
                Fact::EventCount { count: 2 },
                Fact::StateHash { hash: state.hash() },
            ]
        );
    }

    #[test]
    fn screen_observer_tracks_visible_text() {
        let mut observer = ScreenObserver::new(10, 3);
        observer.apply_output(b"hello\r\nworld");

        assert_eq!(observer.screen_text(), "hello\nworld");
        assert!(
            Predicate::ContainsText {
                text: "world".into()
            }
            .matches(&observer.state())
        );
    }

    #[test]
    fn screen_observer_handles_clear_and_cursor() {
        let mut observer = ScreenObserver::new(8, 2);
        observer.apply_output(b"abcdef");
        observer.apply_output(b"\x1b[1;3HXY");
        assert_eq!(observer.screen_text(), "abXYef");

        observer.apply_output(b"\x1b[H\x1b[2J");
        assert_eq!(observer.screen_text(), "");
    }

    #[test]
    fn screen_observer_handles_line_clear_and_scroll() {
        let mut observer = ScreenObserver::new(4, 2);
        observer.apply_output(b"abcd\r\x1b[Kx\r\ny\r\nz");

        assert_eq!(observer.screen_text(), "y\nz");
    }
}
