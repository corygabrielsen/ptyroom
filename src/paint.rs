//! Snapshot → PNG renderer.
//!
//! Three-pass per-row paint:
//!  1. Resolve every cell's fg/bg to concrete RGB given the snapshot's
//!     palette overrides + the layer defaults.
//!  2. Coalesce adjacent same-bg runs into a single rectangle (cuts paint
//!     calls from `cols` to `~runs` per row).
//!  3. Stamp glyphs.
//!
//! Font is bundled (`assets/fonts/DejaVuSansMono.ttf`) and embedded into the
//! binary via `include_bytes!` for cross-machine determinism.

use std::path::Path;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{Rgb, RgbImage};

use crate::color::HexColor;
use crate::snapshot::{Cell, Snapshot};

/// Bundled DejaVu Sans Mono. Embedded for byte-stable rendering.
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

#[derive(Debug, Clone, Copy)]
pub struct PaintConfig {
    pub font_size_px: f32,
    pub padding_px: u32,
    /// Cell width override; `None` derives from the font's `M` advance.
    pub cell_w_px: Option<u32>,
    /// Cell height override; `None` derives from `font_size_px + 2`.
    pub cell_h_px: Option<u32>,
}

impl Default for PaintConfig {
    fn default() -> Self {
        Self { font_size_px: 14.0, padding_px: 12, cell_w_px: None, cell_h_px: None }
    }
}

/// Computed cell dimensions for a given font + config.
#[derive(Debug, Clone, Copy)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// Y offset from cell top to glyph baseline.
    pub baseline: u32,
}

/// Renderer state — owns the font and computed metrics so a single
/// renderer can paint many snapshots cheaply.
pub struct Painter<'a> {
    font: FontRef<'a>,
    scale: PxScale,
    metrics: CellMetrics,
    padding: u32,
}

impl<'a> Painter<'a> {
    pub fn new(font_bytes: &'a [u8], cfg: PaintConfig) -> anyhow::Result<Self> {
        let font = FontRef::try_from_slice(font_bytes)
            .map_err(|e| anyhow::anyhow!("font load failed: {e}"))?;
        let scale = PxScale::from(cfg.font_size_px);
        let scaled = font.as_scaled(scale);
        let cell_w = cfg.cell_w_px.unwrap_or_else(|| {
            scaled.h_advance(font.glyph_id('M')).round() as u32
        });
        let cell_h = cfg.cell_h_px.unwrap_or((cfg.font_size_px + 2.0) as u32);
        let baseline = scaled.ascent().round() as u32;
        Ok(Self {
            font,
            scale,
            metrics: CellMetrics { width: cell_w, height: cell_h, baseline },
            padding: cfg.padding_px,
        })
    }

    pub fn metrics(&self) -> CellMetrics { self.metrics }

    pub fn image_dims(&self, snap: &Snapshot) -> (u32, u32) {
        (
            snap.cols() as u32 * self.metrics.width  + 2 * self.padding,
            snap.rows() as u32 * self.metrics.height + 2 * self.padding,
        )
    }

    pub fn paint(&self, snap: &Snapshot) -> RgbImage {
        let (w, h) = self.image_dims(snap);
        let bg_rgb = Rgb([snap.bg.r(), snap.bg.g(), snap.bg.b()]);
        let mut img = RgbImage::from_pixel(w, h, bg_rgb);

        for (y, row) in snap.grid.iter_rows().enumerate() {
            self.paint_row(&mut img, snap, row, y);
        }
        img
    }

    pub fn save_png(&self, snap: &Snapshot, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let img = self.paint(snap);
        img.save(path.as_ref())?;
        Ok(())
    }

    fn paint_row(
        &self, img: &mut RgbImage, snap: &Snapshot,
        row: &[Option<Cell>], y_idx: usize,
    ) {
        let cy = self.padding + y_idx as u32 * self.metrics.height;

        // Pass 1: resolve every cell's effective fg/bg once.
        let resolved: Vec<Option<ResolvedCell<'_>>> = row.iter()
            .map(|opt| opt.as_ref().map(|c| resolve(c, snap)))
            .collect();

        // Pass 2: paint background runs.
        self.paint_bg_runs(img, &resolved, snap.bg, cy);

        // Pass 3: paint glyphs.
        for (x, slot) in resolved.iter().enumerate() {
            let Some(rc) = slot else { continue };
            let ch = rc.cell.first_char();
            if ch == ' ' || ch == '\0' { continue; }
            let cx = self.padding + x as u32 * self.metrics.width;
            self.draw_glyph(img, ch, cx, cy, rc);
        }
    }

    fn paint_bg_runs(
        &self, img: &mut RgbImage,
        row: &[Option<ResolvedCell<'_>>],
        snap_bg: HexColor, cy: u32,
    ) {
        let cols = row.len() as u32;
        let mut x = 0u32;
        while x < cols {
            let Some(rc) = &row[x as usize] else { x += 1; continue; };
            if rc.bg == snap_bg { x += 1; continue; }
            let run_bg = rc.bg;
            let mut x_end = x + 1;
            while x_end < cols {
                let Some(next) = &row[x_end as usize] else { break };
                if next.bg != run_bg { break; }
                x_end += 1;
            }
            fill_rect(
                img,
                self.padding + x * self.metrics.width,
                cy,
                self.padding + x_end * self.metrics.width,
                cy + self.metrics.height,
                run_bg,
            );
            x = x_end;
        }
    }

    fn draw_glyph(&self, img: &mut RgbImage, ch: char, cx: u32, cy: u32, rc: &ResolvedCell<'_>) {
        // Dim: blend 60% toward bg for the foreground.
        let fg = if rc.cell.is_dim() { mix(rc.fg, rc.bg, 0.6) } else { rc.fg };
        let glyph = self.font.glyph_id(ch).with_scale_and_position(
            self.scale,
            ab_glyph::point(cx as f32, (cy + self.metrics.baseline) as f32),
        );
        let Some(outlined) = self.font.outline_glyph(glyph) else { return };
        let bounds = outlined.px_bounds();
        let (img_w, img_h) = (img.width(), img.height());
        outlined.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if px < 0 || py < 0 { return; }
            let (px, py) = (px as u32, py as u32);
            if px >= img_w || py >= img_h { return; }
            // Composite: linearly blend fg over the existing pixel by coverage.
            let existing = img.get_pixel(px, py);
            let blended = blend(*existing, Rgb([fg.r(), fg.g(), fg.b()]), coverage);
            img.put_pixel(px, py, blended);
        });
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolvedCell<'a> {
    cell: &'a Cell,
    fg: HexColor,
    bg: HexColor,
}

fn resolve<'a>(cell: &'a Cell, snap: &Snapshot) -> ResolvedCell<'a> {
    let (fg, bg) = cell.resolve_layers(snap);
    ResolvedCell { cell, fg, bg }
}

fn fill_rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, c: HexColor) {
    let px = Rgb([c.r(), c.g(), c.b()]);
    let (w, h) = (img.width(), img.height());
    let x1 = x1.min(w);
    let y1 = y1.min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            img.put_pixel(x, y, px);
        }
    }
}

fn blend(under: Rgb<u8>, over: Rgb<u8>, alpha: f32) -> Rgb<u8> {
    let a = alpha.clamp(0.0, 1.0);
    let lerp = |u: u8, o: u8| ((u as f32) * (1.0 - a) + (o as f32) * a).round() as u8;
    Rgb([lerp(under.0[0], over.0[0]), lerp(under.0[1], over.0[1]), lerp(under.0[2], over.0[2])])
}

fn mix(a: HexColor, b: HexColor, t: f32) -> HexColor {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    HexColor::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{CellColor, PaletteOverrides};
    use crate::snapshot::{Cell, Grid};

    fn cell(ch: char, fg: CellColor, bg: CellColor) -> Option<Cell> {
        Some(Cell {
            ch: ch.to_string(), fg, bg,
            bold: 0, dim: 0, italic: 0, underline: 0, inverse: 0,
        })
    }

    #[test]
    fn font_loads() {
        Painter::new(FONT_BYTES, PaintConfig::default()).expect("font loads");
    }

    #[test]
    fn dims_match_grid() {
        let p = Painter::new(FONT_BYTES, PaintConfig::default()).unwrap();
        let snap = Snapshot {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![
                vec![cell('a', CellColor::Default, CellColor::Default); 80];
                30
            ]),
        };
        let m = p.metrics();
        let (w, h) = p.image_dims(&snap);
        assert_eq!(w, 80 * m.width  + 24);
        assert_eq!(h, 30 * m.height + 24);
    }

    #[test]
    fn mix_endpoints() {
        let a = HexColor::from_rgb(0, 0, 0);
        let b = HexColor::from_rgb(255, 255, 255);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
    }

    #[test]
    fn fill_rect_clips_to_bounds() {
        let mut img = RgbImage::new(4, 4);
        fill_rect(&mut img, 2, 2, 100, 100, HexColor::from_rgb(255, 0, 0));
        // The bottom-right 2×2 should be red, top-left 2×2 untouched.
        assert_eq!(img.get_pixel(3, 3), &Rgb([255, 0, 0]));
        assert_eq!(img.get_pixel(0, 0), &Rgb([0, 0, 0]));
    }

    #[test]
    fn paint_produces_image_with_correct_dims() {
        let p = Painter::new(FONT_BYTES, PaintConfig::default()).unwrap();
        let snap = Snapshot {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![vec![cell('h', CellColor::Default, CellColor::Default); 5]]),
        };
        let img = p.paint(&snap);
        let (w, h) = p.image_dims(&snap);
        assert_eq!(img.width(), w);
        assert_eq!(img.height(), h);
    }

    #[test]
    fn paint_is_byte_stable() {
        // Two separate paints of identical input must produce identical bytes.
        let p = Painter::new(FONT_BYTES, PaintConfig::default()).unwrap();
        let snap = Snapshot {
            bg: HexColor::from_rgb(0x1a, 0x1b, 0x26),
            fg: HexColor::from_rgb(0xc0, 0xca, 0xf5),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![
                vec![cell('h', CellColor::Default, CellColor::Default),
                     cell('i', CellColor::Default, CellColor::Default)],
            ]),
        };
        let a = p.paint(&snap);
        let b = p.paint(&snap);
        assert_eq!(a.as_raw(), b.as_raw());
    }
}
