//! Cast → per-frame [`Snapshot`] replay.
//!
//! Drives a [`vt100::Parser`] through every `"o"` event in the cast,
//! capturing screen state after each event. Terminal-default bg/fg
//! and palette overrides come from a sibling [`OscTracker`] that
//! sniffs the same bytes for OSC 10/11/4/110/111/104 sequences (vt100
//! only handles OSC 0/1/2/52, so the rest are our responsibility).
//!
//! Pure function: identical cast bytes produce identical snapshots,
//! regardless of CPU scheduling, wall-clock, or thread interleaving.

pub mod osc_tracker;

use crate::cast::{Cast, EventKind};
use crate::color::{CellColor as SnapCellColor, HexColor, PaletteOverrides};
use crate::encode::TimingEntry;
use crate::recorder::StubColors;
use crate::snapshot::{Cell, Grid, Snapshot};

pub use osc_tracker::OscTracker;

/// Tail-frame dwell. Matches the previous TS implementation: the last
/// frame holds for zero ms in the timing manifest because there's no
/// "next event" timestamp to subtract from.
pub const TAIL_DWELL_MS: u32 = 0;

/// Replay `cast` and emit one snapshot per `"o"` event plus the timing
/// manifest. `defaults` seeds the OSC tracker — [`StubColors::default`]
/// mirrors what the recorder serves to OSC 10/11 query replies and is
/// the right choice for casts produced by this crate.
///
/// # Errors
/// Cast header has zero width or height (otherwise vt100 panics).
pub fn replay(
    cast: &Cast,
    defaults: StubColors,
) -> anyhow::Result<(Vec<Snapshot>, Vec<TimingEntry>)> {
    let cols = u16::try_from(cast.header.width)?;
    let rows = u16::try_from(cast.header.height)?;
    if cols == 0 || rows == 0 {
        anyhow::bail!(
            "cast header has zero dimension: {}x{}",
            cast.header.width,
            cast.header.height
        );
    }

    let mut parser = vt100::Parser::new(rows, cols, 0);
    let mut osc = OscTracker::new(defaults);

    let mut snapshots = Vec::with_capacity(cast.events.len());
    let mut timing = Vec::with_capacity(cast.events.len());

    // Snapshot frame indices (1-based, 4-digit zero-padded) come from
    // the original cast event index — preserves the previous TS
    // implementation's filenames so paint/encode/golden checks line up.
    for (i, event) in cast.events.iter().enumerate() {
        if !matches!(event.kind, EventKind::Output) {
            continue;
        }
        let bytes = event.data.as_bytes();
        parser.process(bytes);
        osc.observe(bytes);

        let snapshot = capture(&parser, &osc);
        snapshots.push(snapshot);

        let frame = format!("{:04}", i + 1);
        let next_t = cast.events.get(i + 1).map(|e| e.time_s);
        let dwell_ms = match next_t {
            Some(t) => {
                // Round + clamp to [1, u32::MAX]. Negative deltas can't
                // happen for in-order cast events; saturate to 1 ms
                // anyway in case of clock drift in malformed casts.
                let delta = ((t - event.time_s) * 1000.0).round();
                if delta < 1.0 {
                    1
                } else if delta >= f64::from(u32::MAX) {
                    u32::MAX
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        delta as u32
                    }
                }
            }
            None => TAIL_DWELL_MS,
        };
        timing.push(TimingEntry { frame, dwell_ms });
    }

    Ok((snapshots, timing))
}

fn capture(parser: &vt100::Parser, osc: &OscTracker) -> Snapshot {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let mut grid: Vec<Vec<Option<Cell>>> = Vec::with_capacity(rows as usize);
    for r in 0..rows {
        let mut row: Vec<Option<Cell>> = Vec::with_capacity(cols as usize);
        for c in 0..cols {
            row.push(cell_from_vt100(screen.cell(r, c), osc));
        }
        grid.push(row);
    }

    let palette = palette_overrides(osc);

    Snapshot {
        bg: osc.bg(),
        fg: osc.fg(),
        palette,
        grid: Grid::new(grid).expect("vt100 screen always returns rectangular grid"),
    }
}

fn cell_from_vt100(cell: Option<&vt100::Cell>, osc: &OscTracker) -> Option<Cell> {
    let cell = cell?;
    let ch = {
        let s = cell.contents();
        if s.is_empty() {
            " ".to_string()
        } else {
            s.to_string()
        }
    };
    let candidate = Cell {
        ch,
        fg: convert_color(cell.fgcolor(), osc),
        bg: convert_color(cell.bgcolor(), osc),
        bold: u8::from(cell.bold()),
        dim: u8::from(cell.dim()),
        italic: u8::from(cell.italic()),
        underline: u8::from(cell.underline()),
        inverse: u8::from(cell.inverse()),
    };
    // Canonicalize: a position carrying no state collapses to `None`,
    // serializing as `null`. Any non-default field forces emission of
    // a Cell, with serde skipping the still-default fields.
    if candidate.is_fully_default() {
        None
    } else {
        Some(candidate)
    }
}

fn convert_color(c: vt100::Color, osc: &OscTracker) -> SnapCellColor {
    match c {
        vt100::Color::Default => SnapCellColor::Default,
        vt100::Color::Rgb(r, g, b) => SnapCellColor::Rgb(HexColor::from_rgb(r, g, b)),
        vt100::Color::Idx(idx) => {
            let fallback = osc.palette().get(&idx).copied();
            SnapCellColor::Palette { idx, fallback }
        }
    }
}

fn palette_overrides(osc: &OscTracker) -> PaletteOverrides {
    let mut p = PaletteOverrides::new();
    for (idx, color) in osc.palette() {
        p.set(*idx, *color);
    }
    p
}
