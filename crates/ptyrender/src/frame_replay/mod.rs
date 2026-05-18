//! Trace → per-frame [`Frame`] replay.
//!
//! Drives a [`vt100::Parser`] through trace output and resize events,
//! capturing screen state after each output event. Terminal-default bg/fg
//! and palette overrides come from a sibling [`OscTracker`] that
//! sniffs the same bytes for OSC 10/11/4/110/111/104 sequences (vt100
//! only handles OSC 0/1/2/52, so the rest are our responsibility).
//!
//! Pure function: identical trace bytes produce identical snapshots,
//! regardless of CPU scheduling, wall-clock, or thread interleaving.

pub mod osc_tracker;

use crate::color::{CellColor as SnapCellColor, HexColor, PaletteOverrides};
use crate::encode::TimingEntry;
use crate::frame::{Cell, Frame, Grid};
use ptytrace::pty::StubColors;
use ptytrace::trace::{EventKind, Trace, TraceHeader};

pub use osc_tracker::OscTracker;

/// Tail-frame dwell. Matches the previous TS implementation: the last
/// frame holds for zero ms in the timing manifest because there's no
/// "next event" timestamp to subtract from.
pub const TAIL_DWELL_MS: u32 = 0;

/// Replay `trace` and emit one snapshot per visible event plus the timing
/// manifest. Output and resize events are visible; input events are not.
/// `defaults` seeds the OSC tracker — [`StubColors::default`]
/// mirrors what the recorder serves to OSC 10/11 query replies and is
/// the right choice for traces produced by this crate.
///
/// ```
/// use ptytrace::trace::{Trace, TraceEvent, TraceHeader, EventKind};
/// use ptytrace::pty::StubColors;
/// use ptyrender::frame_replay::replay;
///
/// let trace = Trace {
///     header: TraceHeader { version: 2, width: 80, height: 24, env: Default::default() },
///     events: vec![TraceEvent {
///         time_s: 0.0,
///         kind: EventKind::Output,
///         data: "hello".into(),
///     }],
/// };
/// let (snaps, timing) = replay(&trace, StubColors::default())?;
/// assert_eq!(snaps.len(), 1);
/// assert_eq!(timing.len(), 1);
/// assert_eq!(snaps[0].row_text(0).unwrap(), "hello");
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
/// Trace header has zero width or height, or a resize event is malformed.
pub fn replay(
    trace: &Trace,
    defaults: StubColors,
) -> anyhow::Result<(Vec<Frame>, Vec<TimingEntry>)> {
    let mut state = ReplayState::from_header(&trace.header, defaults)?;

    let visible_events: Vec<_> = trace
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| is_visible_event(event.kind))
        .collect();
    let mut snapshots = Vec::with_capacity(visible_events.len());
    let mut timing = Vec::with_capacity(visible_events.len());

    // Frame indices (1-based, 4-digit zero-padded) come from
    // the original trace event index — preserves the previous TS
    // implementation's filenames so paint/encode/golden checks line up.
    let mut visible_idx = 0;
    for (i, event) in trace.events.iter().enumerate() {
        match event.kind {
            EventKind::Output => {
                let snapshot = state.process_output(event.data.as_bytes());
                push_visible_frame(
                    &mut snapshots,
                    &mut timing,
                    snapshot,
                    &visible_events,
                    &mut visible_idx,
                    i,
                    event.time_s,
                );
            }
            EventKind::Resize => {
                let (cols, rows) = parse_resize_event(&event.data)?;
                state.resize(cols, rows)?;
                let snapshot = state.snapshot();
                push_visible_frame(
                    &mut snapshots,
                    &mut timing,
                    snapshot,
                    &visible_events,
                    &mut visible_idx,
                    i,
                    event.time_s,
                );
            }
            EventKind::Input => {}
        }
    }

    Ok((snapshots, timing))
}

fn is_visible_event(kind: EventKind) -> bool {
    matches!(kind, EventKind::Output | EventKind::Resize)
}

fn push_visible_frame(
    snapshots: &mut Vec<Frame>,
    timing: &mut Vec<TimingEntry>,
    snapshot: Frame,
    visible_events: &[(usize, &ptytrace::trace::TraceEvent)],
    visible_idx: &mut usize,
    event_idx: usize,
    time_s: f64,
) {
    snapshots.push(snapshot);
    let frame = format!("{:04}", event_idx + 1);
    let next_t = visible_events
        .get(*visible_idx + 1)
        .map(|(_, next_event)| next_event.time_s);
    timing.push(TimingEntry {
        frame,
        dwell_ms: dwell_until_next_visible_event(time_s, next_t),
    });
    *visible_idx += 1;
}

fn dwell_until_next_visible_event(time_s: f64, next_visible_time_s: Option<f64>) -> u32 {
    match next_visible_time_s {
        Some(t) => {
            // Round + clamp to [1, u32::MAX]. Negative deltas can't
            // happen for in-order trace events; saturate to 1 ms
            // anyway in case of clock drift in malformed traces.
            let delta = ((t - time_s) * 1000.0).round();
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
    }
}

/// Largest cols/rows a trace resize event is allowed to specify.
/// Bounds the vt100 grid allocation a pathological or hostile trace
/// can force. Any sensible terminal is well below this.
const MAX_TRACE_DIM: u16 = 1024;

fn parse_resize_event(data: &str) -> anyhow::Result<(u16, u16)> {
    let Some((cols, rows)) = data.split_once('x') else {
        anyhow::bail!("malformed resize event: {data:?}");
    };
    let cols = cols.parse::<u16>()?;
    let rows = rows.parse::<u16>()?;
    if cols == 0 || rows == 0 {
        anyhow::bail!("resize event has zero dimension: {cols}x{rows}");
    }
    if cols > MAX_TRACE_DIM || rows > MAX_TRACE_DIM {
        anyhow::bail!("resize event exceeds max dimension {MAX_TRACE_DIM}: {cols}x{rows}");
    }
    Ok((cols, rows))
}

/// Incremental trace replay state.
///
/// This is the shared terminal model used by both batch rendering and
/// live `.ptyrecord` stitching. Feeding the same output bytes through
/// this state yields the same frames as [`replay`].
pub struct ReplayState {
    parser: vt100::Parser,
    osc: OscTracker,
}

impl ReplayState {
    /// Build replay state for a trace header.
    ///
    /// # Errors
    /// Header dimensions do not fit in `u16` or either dimension is zero.
    pub fn from_header(header: &TraceHeader, defaults: StubColors) -> anyhow::Result<Self> {
        let cols = u16::try_from(header.width)?;
        let rows = u16::try_from(header.height)?;
        Self::new(cols, rows, defaults)
    }

    /// Build replay state for explicit terminal geometry.
    ///
    /// # Errors
    /// Either dimension is zero.
    pub fn new(cols: u16, rows: u16, defaults: StubColors) -> anyhow::Result<Self> {
        if cols == 0 || rows == 0 {
            anyhow::bail!("trace header has zero dimension: {cols}x{rows}");
        }
        if cols > MAX_TRACE_DIM || rows > MAX_TRACE_DIM {
            anyhow::bail!("trace header exceeds max dimension {MAX_TRACE_DIM}: {cols}x{rows}");
        }

        Ok(Self {
            parser: vt100::Parser::new(rows, cols, 0),
            osc: OscTracker::new(defaults),
        })
    }

    /// Apply one output chunk and return the resulting visible frame.
    #[must_use]
    pub fn process_output(&mut self, bytes: &[u8]) -> Frame {
        self.parser.process(bytes);
        self.osc.observe(bytes);
        self.snapshot()
    }

    #[must_use]
    pub fn snapshot(&self) -> Frame {
        capture(&self.parser, &self.osc)
    }

    /// Apply a trace resize event to the terminal model.
    ///
    /// # Errors
    /// Either dimension is zero.
    pub fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()> {
        if cols == 0 || rows == 0 {
            anyhow::bail!("resize has zero dimension: {cols}x{rows}");
        }
        if cols > MAX_TRACE_DIM || rows > MAX_TRACE_DIM {
            anyhow::bail!("resize exceeds max dimension {MAX_TRACE_DIM}: {cols}x{rows}");
        }
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }
}

fn capture(parser: &vt100::Parser, osc: &OscTracker) -> Frame {
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

    Frame {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ptytrace::trace::TraceEvent;

    #[test]
    fn dwell_uses_next_output_event_not_next_trace_event() {
        let trace = Trace {
            header: TraceHeader {
                version: 2,
                width: 20,
                height: 4,
                env: std::collections::BTreeMap::default(),
            },
            events: vec![
                TraceEvent {
                    time_s: 0.0,
                    kind: EventKind::Output,
                    data: "hello".into(),
                },
                TraceEvent {
                    time_s: 0.1,
                    kind: EventKind::Input,
                    data: "ignored for frame timing".into(),
                },
                TraceEvent {
                    time_s: 1.0,
                    kind: EventKind::Output,
                    data: " world".into(),
                },
            ],
        };

        let (_, timing) = replay(&trace, StubColors::default()).unwrap();

        assert_eq!(timing.len(), 2);
        assert_eq!(timing[0].frame, "0001");
        assert_eq!(timing[0].dwell_ms, 1000);
        assert_eq!(timing[1].frame, "0003");
        assert_eq!(timing[1].dwell_ms, TAIL_DWELL_MS);
    }

    #[test]
    fn resize_event_changes_replay_geometry_before_next_output() {
        let trace = Trace {
            header: TraceHeader {
                version: 2,
                width: 10,
                height: 2,
                env: std::collections::BTreeMap::default(),
            },
            events: vec![
                TraceEvent {
                    time_s: 0.0,
                    kind: EventKind::Resize,
                    data: "4x2".into(),
                },
                TraceEvent {
                    time_s: 0.1,
                    kind: EventKind::Output,
                    data: "abcdef".into(),
                },
            ],
        };

        let (snapshots, timing) = replay(&trace, StubColors::default()).unwrap();

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].cols(), 4);
        assert_eq!(snapshots[1].cols(), 4);
        assert_eq!(timing[0].frame, "0001");
        assert_eq!(timing[0].dwell_ms, 100);
        assert_eq!(timing[1].frame, "0002");
    }

    #[test]
    fn resize_between_outputs_is_a_visible_timing_boundary() {
        let trace = Trace {
            header: TraceHeader {
                version: 2,
                width: 10,
                height: 2,
                env: std::collections::BTreeMap::default(),
            },
            events: vec![
                TraceEvent {
                    time_s: 0.0,
                    kind: EventKind::Output,
                    data: "abcdef".into(),
                },
                TraceEvent {
                    time_s: 0.5,
                    kind: EventKind::Resize,
                    data: "4x2".into(),
                },
                TraceEvent {
                    time_s: 1.0,
                    kind: EventKind::Output,
                    data: "Z".into(),
                },
            ],
        };

        let (snapshots, timing) = replay(&trace, StubColors::default()).unwrap();

        assert_eq!(snapshots.len(), 3);
        assert_eq!(snapshots[0].cols(), 10);
        assert_eq!(snapshots[1].cols(), 4);
        assert_eq!(timing[0].dwell_ms, 500);
        assert_eq!(timing[1].dwell_ms, 500);
        assert_eq!(timing[2].dwell_ms, TAIL_DWELL_MS);
    }
}
