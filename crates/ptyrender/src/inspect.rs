//! ASCII inspector — render a `Frame` to a terminal as either plain text
//! or true-color ANSI for visual debugging.
//!
//! Mirrors `paint.py`'s color resolution path, so what you see in the
//! terminal matches what `paint.rs` would emit to PNG.

use crate::frame::{Cell, Frame};

/// Half-open `[start, end)` row range; out-of-range bounds clamp.
#[derive(Debug, Clone, Copy)]
pub struct RowRange {
    pub start: usize,
    pub end: usize,
}

impl RowRange {
    /// Parse `start:end`, `:end`, `start:`, or `N` (single row).
    /// Out-of-range values clamp; never panics.
    ///
    /// # Errors
    /// Either side of the colon (or the whole spec for the single-row form)
    /// fails to parse as `usize`.
    pub fn parse(spec: &str, total: usize) -> Result<Self, String> {
        let clamp = |n: usize| n.min(total);
        let Some((a, b)) = spec.split_once(':') else {
            let n: usize = spec
                .parse()
                .map_err(|_| format!("not a number: {spec:?}"))?;
            let n = clamp(n);
            return Ok(Self {
                start: n,
                end: clamp(n + 1),
            });
        };
        let start = if a.is_empty() {
            0
        } else {
            clamp(a.parse().map_err(|_| format!("not a number: {a:?}"))?)
        };
        let end = if b.is_empty() {
            total
        } else {
            clamp(b.parse().map_err(|_| format!("not a number: {b:?}"))?)
        };
        Ok(Self {
            start,
            end: end.max(start),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InspectMode {
    Plain,
    /// Emit ANSI true-color escapes per cell.
    Color,
}

/// Render `snap` to lines numbered with their 1-based row index.
#[must_use]
pub fn render(snap: &Frame, range: RowRange, mode: InspectMode) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let width = snap.rows().to_string().len();
    for y in range.start..range.end {
        let line = match mode {
            InspectMode::Plain => render_row_plain(snap, y),
            InspectMode::Color => render_row_color(snap, y),
        };
        writeln!(out, "{:>width$}  {line}", y + 1).expect("write to String cannot fail");
    }
    out
}

fn render_row_plain(snap: &Frame, y: usize) -> String {
    snap.grid
        .row(y)
        .map(|row| {
            row.iter()
                .map(|c| c.as_ref().map_or(' ', Cell::first_char))
                .collect()
        })
        .unwrap_or_default()
}

fn render_row_color(snap: &Frame, y: usize) -> String {
    use std::fmt::Write as _;
    let Some(row) = snap.grid.row(y) else {
        return String::new();
    };
    let mut out = String::new();
    for cell in row {
        match cell {
            None => out.push(' '),
            Some(c) => {
                // Goes through Cell::resolve_layers so the inverse attribute
                // applies here just like it does in the PNG renderer.
                let (fg, bg) = c.resolve_layers(snap);
                write!(out, "\x1b[48;2;{};{};{}m", bg.r(), bg.g(), bg.b())
                    .expect("write to String cannot fail");
                write!(out, "\x1b[38;2;{};{};{}m", fg.r(), fg.g(), fg.b())
                    .expect("write to String cannot fail");
                out.push(c.first_char());
            }
        }
    }
    out.push_str("\x1b[0m");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_range_parses_full() {
        let r = RowRange::parse(":", 10).unwrap();
        assert_eq!(r.start, 0);
        assert_eq!(r.end, 10);
    }

    #[test]
    fn row_range_parses_bounded() {
        let r = RowRange::parse("3:7", 10).unwrap();
        assert_eq!(r.start, 3);
        assert_eq!(r.end, 7);
    }

    #[test]
    fn row_range_parses_open_start() {
        let r = RowRange::parse(":5", 10).unwrap();
        assert_eq!(r.start, 0);
        assert_eq!(r.end, 5);
    }

    #[test]
    fn row_range_parses_open_end() {
        let r = RowRange::parse("5:", 10).unwrap();
        assert_eq!(r.start, 5);
        assert_eq!(r.end, 10);
    }

    #[test]
    fn row_range_parses_single() {
        let r = RowRange::parse("4", 10).unwrap();
        assert_eq!(r.start, 4);
        assert_eq!(r.end, 5);
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
