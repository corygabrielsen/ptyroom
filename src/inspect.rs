//! ASCII inspector — render a `Snapshot` to a terminal as either plain text
//! or true-color ANSI for visual debugging.
//!
//! Mirrors `paint.py`'s color resolution path, so what you see in the
//! terminal matches what `paint.rs` would emit to PNG.

use crate::color::HexColor;
use crate::snapshot::Snapshot;
#[allow(unused_imports)]
use crate::snapshot::Cell;

/// Half-open `[start, end)` row range; out-of-range bounds clamp.
#[derive(Debug, Clone, Copy)]
pub struct RowRange {
    pub start: usize,
    pub end: usize,
}

impl RowRange {
    pub fn full(snap: &Snapshot) -> Self {
        Self { start: 0, end: snap.rows() }
    }

    /// Parse `start:end`, `:end`, `start:`, or `N` (single row).
    /// Out-of-range values clamp; never panics.
    pub fn parse(spec: &str, total: usize) -> Result<Self, String> {
        let clamp = |n: usize| n.min(total);
        if !spec.contains(':') {
            let n: usize = spec.parse().map_err(|_| format!("not a number: {spec:?}"))?;
            let n = clamp(n);
            return Ok(Self { start: n, end: clamp(n + 1) });
        }
        let (a, b) = spec.split_once(':').unwrap();
        let start = if a.is_empty() { 0 } else {
            clamp(a.parse().map_err(|_| format!("not a number: {a:?}"))?)
        };
        let end = if b.is_empty() { total } else {
            clamp(b.parse().map_err(|_| format!("not a number: {b:?}"))?)
        };
        Ok(Self { start, end: end.max(start) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectMode {
    Plain,
    /// Emit ANSI true-color escapes per cell.
    Color,
}

/// Render `snap` to lines numbered with their 1-based row index.
pub fn render(snap: &Snapshot, range: RowRange, mode: InspectMode) -> String {
    let mut out = String::new();
    let width = snap.rows().to_string().len();
    for y in range.start..range.end {
        let line = match mode {
            InspectMode::Plain => render_row_plain(snap, y),
            InspectMode::Color => render_row_color(snap, y),
        };
        out.push_str(&format!("{:>width$}  {}\n", y + 1, line, width = width));
    }
    out
}

fn render_row_plain(snap: &Snapshot, y: usize) -> String {
    snap.grid.row(y).map(|row| {
        row.iter().map(|c| c.as_ref().map(Cell::first_char).unwrap_or(' ')).collect()
    }).unwrap_or_default()
}

fn render_row_color(snap: &Snapshot, y: usize) -> String {
    let row = match snap.grid.row(y) { Some(r) => r, None => return String::new() };
    let mut out = String::new();
    for cell in row {
        match cell {
            None => out.push(' '),
            Some(c) => {
                let bg = c.bg.resolve(snap.bg, &snap.palette);
                let fg = c.fg.resolve(snap.fg, &snap.palette);
                push_ansi_bg(&mut out, bg);
                push_ansi_fg(&mut out, fg);
                out.push(c.first_char());
            }
        }
    }
    out.push_str("\x1b[0m");
    out
}

fn push_ansi_bg(s: &mut String, c: HexColor) {
    s.push_str(&format!("\x1b[48;2;{};{};{}m", c.r(), c.g(), c.b()));
}
fn push_ansi_fg(s: &mut String, c: HexColor) {
    s.push_str(&format!("\x1b[38;2;{};{};{}m", c.r(), c.g(), c.b()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_range_parses_full() {
        let r = RowRange::parse(":", 10).unwrap();
        assert_eq!(r.start, 0); assert_eq!(r.end, 10);
    }

    #[test]
    fn row_range_parses_bounded() {
        let r = RowRange::parse("3:7", 10).unwrap();
        assert_eq!(r.start, 3); assert_eq!(r.end, 7);
    }

    #[test]
    fn row_range_parses_open_start() {
        let r = RowRange::parse(":5", 10).unwrap();
        assert_eq!(r.start, 0); assert_eq!(r.end, 5);
    }

    #[test]
    fn row_range_parses_open_end() {
        let r = RowRange::parse("5:", 10).unwrap();
        assert_eq!(r.start, 5); assert_eq!(r.end, 10);
    }

    #[test]
    fn row_range_parses_single() {
        let r = RowRange::parse("4", 10).unwrap();
        assert_eq!(r.start, 4); assert_eq!(r.end, 5);
    }

    #[test]
    fn row_range_clamps_overflow() {
        let r = RowRange::parse("0:999", 10).unwrap();
        assert_eq!(r.end, 10);
    }

    #[test]
    fn row_range_rejects_garbage() {
        assert!(RowRange::parse("abc", 10).is_err());
        assert!(RowRange::parse("a:b", 10).is_err());
    }
}
