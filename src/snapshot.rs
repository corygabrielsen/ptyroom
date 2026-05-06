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

/// A single terminal cell. Sparsely serialized: any field equal to its
/// default is elided from the JSON output, so the wire form for a
/// fully-default cell is `{}`. Producers must collapse fully-default
/// cells to `None` (see [`Grid`]) so the wire form for an untouched
/// position is the shorter `null`.
///
/// The struct field order is the canonical JSON field order
/// (serde_json honors derive order). Do not reorder fields without
/// re-blessing goldens.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme. Usually one codepoint; combining marks land here too.
    /// Default is a single space — equivalent to "blank cell."
    #[serde(default = "default_ch", skip_serializing_if = "is_default_ch")]
    pub ch: String,
    #[serde(default, skip_serializing_if = "CellColor::is_default")]
    pub fg: CellColor,
    #[serde(default, skip_serializing_if = "CellColor::is_default")]
    pub bg: CellColor,
    /// Boolean attribute flags, encoded as 0/1 to mirror the JSON wire format.
    /// Use the `is_*` accessors for typed access.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub bold: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub dim: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub italic: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub underline: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub inverse: u8,
}

fn default_ch() -> String {
    " ".into()
}

fn is_default_ch(s: &str) -> bool {
    s == " "
}

fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: default_ch(),
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: 0,
            dim: 0,
            italic: 0,
            underline: 0,
            inverse: 0,
        }
    }
}

impl Cell {
    /// True if every field equals [`Cell::default`]. Producers should
    /// collapse such cells to [`None`] in the grid so the JSON wire
    /// form is the shorter `null` rather than `{}`.
    #[must_use]
    pub fn is_fully_default(&self) -> bool {
        self == &Cell::default()
    }

    #[must_use]
    pub fn is_bold(&self) -> bool {
        self.bold != 0
    }
    #[must_use]
    pub fn is_dim(&self) -> bool {
        self.dim != 0
    }
    #[must_use]
    pub fn is_italic(&self) -> bool {
        self.italic != 0
    }
    #[must_use]
    pub fn is_underline(&self) -> bool {
        self.underline != 0
    }
    #[must_use]
    pub fn is_inverse(&self) -> bool {
        self.inverse != 0
    }

    /// First grapheme as a `char`, or space if the cell is empty/multi-byte.
    /// Used for ASCII row dumps where we want one column per cell.
    #[must_use]
    pub fn first_char(&self) -> char {
        self.ch.chars().next().unwrap_or(' ')
    }

    /// Resolve this cell's `(fg, bg)` to concrete RGB given the snapshot's
    /// layer defaults and palette overrides, applying the `inverse`
    /// attribute as a final swap. Single source of truth — both the PNG
    /// renderer and the ASCII inspector go through here.
    #[must_use]
    pub fn resolve_layers(&self, snap: &Snapshot) -> (HexColor, HexColor) {
        let mut fg = self.fg.resolve(snap.fg, &snap.palette);
        let mut bg = self.bg.resolve(snap.bg, &snap.palette);
        if self.is_inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        (fg, bg)
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
    /// Read and validate a snapshot JSON file.
    ///
    /// # Errors
    /// IO error, JSON parse error, or non-rectangular/empty grid.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path.as_ref())?;
        let snap: Snapshot = serde_json::from_slice(&bytes)?;
        snap.validate()?;
        Ok(snap)
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.grid.rows()
    }
    #[must_use]
    pub fn cols(&self) -> usize {
        self.grid.cols()
    }

    /// Render row `y` as a `String` of `first_char()` per cell, right-trimmed.
    /// Returns `None` if `y` is out of range.
    #[must_use]
    pub fn row_text(&self, y: usize) -> Option<String> {
        let row = self.grid.row(y)?;
        let mut s: String = row
            .iter()
            .map(|c| c.as_ref().map_or(' ', Cell::first_char))
            .collect();
        let trimmed_len = s.trim_end().len();
        s.truncate(trimmed_len);
        Some(s)
    }

    fn validate(&self) -> anyhow::Result<()> {
        self.grid.validate()
    }
}

/// Rectangular grid of optional cells. `None` is the canonical
/// representation of a cell carrying no state at all — no character,
/// no fg/bg, no SGR attrs. Producers must collapse fully-default
/// cells to `None` (see [`Cell::is_fully_default`]) so the JSON
/// wire form stays compact and emulator-stable.
///
/// The inner `Vec` is private — every `Grid` either came from the
/// validating [`Grid::new`] constructor or from JSON deserialization that
/// also runs [`Grid::validate`]. Callers can't construct a non-rectangular
/// grid, so [`Snapshot::row_text`] and the renderer can iterate without
/// width-mismatch defense.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Grid(Vec<Vec<Option<Cell>>>);

impl Grid {
    /// Validating constructor.
    ///
    /// # Errors
    /// Empty grid (zero rows or zero cols), or any row whose length
    /// differs from the first row's length.
    pub fn new(rows: Vec<Vec<Option<Cell>>>) -> anyhow::Result<Self> {
        let g = Grid(rows);
        g.validate()?;
        Ok(g)
    }

    /// Test-only escape hatch — accept any grid (including ragged or empty).
    /// Tests sometimes exercise validation by passing in a bad grid.
    #[cfg(test)]
    pub(crate) fn from_unchecked(rows: Vec<Vec<Option<Cell>>>) -> Self {
        Grid(rows)
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.0.len()
    }
    pub fn cols(&self) -> usize {
        self.0.first().map_or(0, Vec::len)
    }

    pub fn row(&self, y: usize) -> Option<&[Option<Cell>]> {
        self.0.get(y).map(Vec::as_slice)
    }

    #[must_use]
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
                    row.len(),
                    cols,
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_cell(ch: char) -> Cell {
        Cell {
            ch: ch.to_string(),
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: 0,
            dim: 0,
            italic: 0,
            underline: 0,
            inverse: 0,
        }
    }

    #[test]
    fn cell_first_char_handles_empty_string() {
        let c = Cell {
            ch: String::new(),
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: 0,
            dim: 0,
            italic: 0,
            underline: 0,
            inverse: 0,
        };
        assert_eq!(c.first_char(), ' ');
    }

    #[test]
    fn cell_first_char_takes_first_codepoint() {
        let c = Cell {
            ch: "👋".into(),
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: 0,
            dim: 0,
            italic: 0,
            underline: 0,
            inverse: 0,
        };
        assert_eq!(c.first_char(), '👋');
    }

    #[test]
    fn grid_rejects_non_rectangular() {
        let g = Grid::from_unchecked(vec![
            vec![Some(solid_cell('a')), Some(solid_cell('b'))],
            vec![Some(solid_cell('c'))], // ragged
        ]);
        assert!(g.validate().is_err());
    }

    #[test]
    fn grid_rejects_empty() {
        let g = Grid::from_unchecked(vec![]);
        assert!(g.validate().is_err());
    }

    #[test]
    fn snapshot_row_text_right_trims() {
        let g = Grid::from_unchecked(vec![vec![
            Some(solid_cell('h')),
            Some(solid_cell('i')),
            Some(solid_cell(' ')),
            Some(solid_cell(' ')),
        ]]);
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
        let g = Grid::from_unchecked(vec![vec![
            Some(solid_cell('a')),
            None,
            Some(solid_cell('b')),
        ]]);
        let s = Snapshot {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: g,
        };
        assert_eq!(s.row_text(0).unwrap(), "a b");
    }

    #[test]
    fn cell_default_serializes_to_empty_object() {
        let s = serde_json::to_string(&Cell::default()).unwrap();
        assert_eq!(s, "{}");
    }

    #[test]
    fn cell_default_round_trips_from_empty_object() {
        let c: Cell = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Cell::default());
    }

    #[test]
    fn cell_default_round_trips_from_legacy_dense_form() {
        // Dense form is what older snapshot drivers produced. The new
        // schema must continue to deserialize it identically so old
        // goldens keep working if anyone re-runs against them.
        let dense = r#"{"ch":" ","fg":null,"bg":null,"bold":0,"dim":0,"italic":0,"underline":0,"inverse":0}"#;
        let c: Cell = serde_json::from_str(dense).unwrap();
        assert_eq!(c, Cell::default());
    }

    #[test]
    fn cell_with_only_bg_serializes_sparsely() {
        let c = Cell {
            ch: " ".into(),
            bg: CellColor::Rgb(HexColor::from_rgb(0x12, 0x34, 0x56)),
            ..Cell::default()
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, "{\"bg\":\"#123456\"}");
    }

    #[test]
    fn fully_default_cell_is_fully_default() {
        assert!(Cell::default().is_fully_default());
        assert!(
            !Cell {
                ch: "a".into(),
                ..Cell::default()
            }
            .is_fully_default()
        );
    }

    #[test]
    fn snapshot_json_round_trip() {
        let s = Snapshot {
            bg: HexColor::from_rgb(0x1a, 0x1b, 0x26),
            fg: HexColor::from_rgb(0xc0, 0xca, 0xf5),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![vec![Some(solid_cell('x'))]]),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rows(), 1);
        assert_eq!(back.cols(), 1);
        assert_eq!(back.bg, s.bg);
        assert_eq!(back.fg, s.fg);
    }
}
