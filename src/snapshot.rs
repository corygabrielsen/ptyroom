//! Snapshot data: per-frame terminal state captured by `snapshot.ts`.
//!
//! Each `Snapshot` encodes the state visible after one cast event:
//! terminal-default bg/fg, the OSC 4 palette overrides, and a `cols × rows`
//! grid of [`Cell`]s. `Snapshot::load` reads the JSON written by
//! `renderer/snapshot.ts`.
//!
//! Invariants enforced by the constructors:
//! - `grid` is rectangular: every row has the same `cols`
//! - `rows() > 0` and `cols() > 0`

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::color::{CellColor, HexColor, PaletteOverrides};

/// A single terminal cell.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme. Usually one codepoint; combining marks land here too.
    pub ch: String,
    pub fg: CellColor,
    pub bg: CellColor,
    /// Boolean attribute flags, encoded as 0/1 to mirror the JSON wire format.
    /// Use the `is_*` accessors for typed access.
    #[serde(default)] pub bold: u8,
    #[serde(default)] pub dim: u8,
    #[serde(default)] pub italic: u8,
    #[serde(default)] pub underline: u8,
    #[serde(default)] pub inverse: u8,
}

impl Cell {
    pub fn is_bold(&self)      -> bool { self.bold != 0 }
    pub fn is_dim(&self)       -> bool { self.dim != 0 }
    pub fn is_italic(&self)    -> bool { self.italic != 0 }
    pub fn is_underline(&self) -> bool { self.underline != 0 }
    pub fn is_inverse(&self)   -> bool { self.inverse != 0 }

    /// First grapheme as a `char`, or space if the cell is empty/multi-byte.
    /// Used for ASCII row dumps where we want one column per cell.
    pub fn first_char(&self) -> char {
        self.ch.chars().next().unwrap_or(' ')
    }
}

/// Captured frame: bg/fg/palette state + a rectangular grid.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Snapshot {
    pub bg: HexColor,
    pub fg: HexColor,
    #[serde(default)]
    pub palette: PaletteOverrides,
    pub grid: Grid,
}

impl Snapshot {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path.as_ref())?;
        let snap: Snapshot = serde_json::from_slice(&bytes)?;
        snap.validate()?;
        Ok(snap)
    }

    pub fn rows(&self) -> usize { self.grid.rows() }
    pub fn cols(&self) -> usize { self.grid.cols() }

    /// Render row `y` as a `String` of `first_char()` per cell, right-trimmed.
    /// Returns `None` if `y` is out of range.
    pub fn row_text(&self, y: usize) -> Option<String> {
        let row = self.grid.row(y)?;
        let mut s: String = row.iter()
            .map(|c| c.as_ref().map(Cell::first_char).unwrap_or(' '))
            .collect();
        let trimmed_len = s.trim_end().len();
        s.truncate(trimmed_len);
        Some(s)
    }

    fn validate(&self) -> anyhow::Result<()> {
        self.grid.validate()
    }
}

/// Rectangular grid of optional cells. `None` is rare — empty cells where
/// the terminal emulator has no content. Snapshots from `@xterm/headless`
/// usually fill all cells with at least a space, but we accept absence.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Grid(pub Vec<Vec<Option<Cell>>>);

impl Grid {
    pub fn rows(&self) -> usize { self.0.len() }
    pub fn cols(&self) -> usize { self.0.first().map(Vec::len).unwrap_or(0) }

    pub fn row(&self, y: usize) -> Option<&[Option<Cell>]> {
        self.0.get(y).map(Vec::as_slice)
    }

    pub fn cell(&self, x: usize, y: usize) -> Option<&Cell> {
        self.row(y)?.get(x)?.as_ref()
    }

    pub fn iter_rows(&self) -> impl Iterator<Item = &[Option<Cell>]> {
        self.0.iter().map(Vec::as_slice)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let cols = self.cols();
        if self.rows() == 0 || cols == 0 {
            anyhow::bail!("snapshot grid is empty");
        }
        for (y, row) in self.0.iter().enumerate() {
            if row.len() != cols {
                anyhow::bail!(
                    "snapshot grid not rectangular: row {y} has {} cells, expected {}",
                    row.len(), cols,
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_cell(ch: char) -> Option<Cell> {
        Some(Cell {
            ch: ch.to_string(),
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: 0, dim: 0, italic: 0, underline: 0, inverse: 0,
        })
    }

    #[test]
    fn cell_first_char_handles_empty_string() {
        let c = Cell { ch: String::new(), fg: CellColor::Default, bg: CellColor::Default,
            bold:0, dim:0, italic:0, underline:0, inverse:0 };
        assert_eq!(c.first_char(), ' ');
    }

    #[test]
    fn cell_first_char_takes_first_codepoint() {
        let c = Cell { ch: "👋".into(), fg: CellColor::Default, bg: CellColor::Default,
            bold:0, dim:0, italic:0, underline:0, inverse:0 };
        assert_eq!(c.first_char(), '👋');
    }

    #[test]
    fn grid_rejects_non_rectangular() {
        let g = Grid(vec![
            vec![solid_cell('a'), solid_cell('b')],
            vec![solid_cell('c')], // ragged
        ]);
        assert!(g.validate().is_err());
    }

    #[test]
    fn grid_rejects_empty() {
        let g = Grid(vec![]);
        assert!(g.validate().is_err());
    }

    #[test]
    fn snapshot_row_text_right_trims() {
        let g = Grid(vec![
            vec![solid_cell('h'), solid_cell('i'), solid_cell(' '), solid_cell(' ')],
        ]);
        let s = Snapshot {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: g,
        };
        assert_eq!(s.row_text(0).unwrap(), "hi");
    }

    #[test]
    fn snapshot_row_text_handles_none_cells_as_space() {
        let g = Grid(vec![
            vec![solid_cell('a'), None, solid_cell('b')],
        ]);
        let s = Snapshot {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: g,
        };
        assert_eq!(s.row_text(0).unwrap(), "a b");
    }

    #[test]
    fn snapshot_json_round_trip() {
        let s = Snapshot {
            bg: HexColor::from_rgb(0x1a, 0x1b, 0x26),
            fg: HexColor::from_rgb(0xc0, 0xca, 0xf5),
            palette: PaletteOverrides::new(),
            grid: Grid(vec![vec![solid_cell('x')]]),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rows(), 1);
        assert_eq!(back.cols(), 1);
        assert_eq!(back.bg, s.bg);
        assert_eq!(back.fg, s.fg);
    }
}
