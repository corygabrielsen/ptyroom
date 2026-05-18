//! Frame → PNG renderer.
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
use anyhow::Context;
use image::{Rgb, RgbImage};

use crate::color::{CellColor, HexColor, PaletteOverrides};
use crate::frame::{Cell, Frame};

/// Bundled `DejaVu` Sans Mono. Embedded for byte-stable rendering.
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

/// Knobs for how a [`Painter`] sizes its output: font size, image padding,
/// and optional cell-dimension overrides.
#[derive(Debug, Clone, Copy)]
pub struct PaintConfig {
    /// Font height in pixels passed to the rasterizer.
    pub font_size_px: f32,
    /// Outer padding (in pixels) applied to all four sides of the image.
    pub padding_px: u32,
    /// Cell width override; `None` derives from the font's `M` advance.
    pub cell_w_px: Option<u32>,
    /// Cell height override; `None` derives from `font_size_px + 2`.
    pub cell_h_px: Option<u32>,
}

impl Default for PaintConfig {
    fn default() -> Self {
        Self {
            font_size_px: 14.0,
            padding_px: 12,
            cell_w_px: None,
            cell_h_px: None,
        }
    }
}

/// Computed cell dimensions for a given font + config.
#[derive(Debug, Clone, Copy)]
pub struct CellMetrics {
    /// Cell width in pixels.
    pub width: u32,
    /// Cell height in pixels.
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
    /// # Errors
    /// Font fails to parse (corrupt or unsupported format).
    pub fn new(font_bytes: &'a [u8], cfg: PaintConfig) -> anyhow::Result<Self> {
        let font = FontRef::try_from_slice(font_bytes)
            .map_err(|e| anyhow::anyhow!("font load failed: {e}"))?;
        let scale = PxScale::from(cfg.font_size_px);
        let scaled = font.as_scaled(scale);
        let cell_w = cfg
            .cell_w_px
            .unwrap_or_else(|| f32_round_to_u32(scaled.h_advance(font.glyph_id('M'))));
        let cell_h = cfg
            .cell_h_px
            .unwrap_or_else(|| f32_floor_to_u32(cfg.font_size_px + 2.0));
        let baseline = f32_round_to_u32(scaled.ascent());
        Ok(Self {
            font,
            scale,
            metrics: CellMetrics {
                width: cell_w,
                height: cell_h,
                baseline,
            },
            padding: cfg.padding_px,
        })
    }

    /// Computed cell metrics (cell width/height, baseline) for this painter.
    #[must_use]
    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// `(width, height)` in pixels of the image that [`Self::paint`] will
    /// produce for `snap`, including padding.
    #[must_use]
    pub fn image_dims(&self, snap: &Frame) -> (u32, u32) {
        (
            usize_to_u32(snap.cols()) * self.metrics.width + 2 * self.padding,
            usize_to_u32(snap.rows()) * self.metrics.height + 2 * self.padding,
        )
    }

    /// Render `snap` to an in-memory RGB image. Pure function of
    /// `(self, snap)` — no painter state mutates.
    #[must_use]
    pub fn paint(&self, snap: &Frame) -> RgbImage {
        let (w, h) = self.image_dims(snap);
        let bg_rgb = Rgb([snap.bg.r(), snap.bg.g(), snap.bg.b()]);
        let mut img = RgbImage::from_pixel(w, h, bg_rgb);

        // Build a flat 256-slot palette lookup once per frame so
        // per-cell palette resolution is an O(1) array index instead
        // of the linear scan inside `PaletteOverrides::get`.
        let palette = PaletteLookup::build(&snap.palette);

        // Hoist the resolved-cell scratch buffer out of the row loop
        // so it's allocated once per frame instead of once per row.
        // Rows share width; `clear` + `extend` reuses the existing
        // capacity without reallocating.
        let mut resolved: Vec<Option<ResolvedCell<'_>>> = Vec::with_capacity(snap.cols());

        for (y, row) in snap.grid.iter_rows().enumerate() {
            self.paint_row(&mut img, snap, &palette, row, y, &mut resolved);
        }
        img
    }

    /// # Errors
    /// IO error writing the PNG.
    pub fn save_png(&self, snap: &Frame, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        let img = self.paint(snap);
        img.save(path)
            .with_context(|| format!("save_png {}", path.display()))?;
        Ok(())
    }

    fn paint_row<'b>(
        &self,
        img: &mut RgbImage,
        snap: &'b Frame,
        palette: &PaletteLookup,
        row: &'b [Option<Cell>],
        y_idx: usize,
        resolved: &mut Vec<Option<ResolvedCell<'b>>>,
    ) {
        let cy = self.padding + usize_to_u32(y_idx) * self.metrics.height;

        // Pass 1: resolve every cell's effective fg/bg once into the
        // shared scratch buffer.
        resolved.clear();
        resolved.extend(
            row.iter()
                .map(|opt| opt.as_ref().map(|c| resolve(c, snap, palette))),
        );

        // Pass 2: paint background runs.
        self.paint_bg_runs(img, resolved, snap.bg, cy);

        // Pass 3: paint glyphs.
        for (x, slot) in resolved.iter().enumerate() {
            let Some(rc) = slot else { continue };
            let ch = rc.cell.first_char();
            if ch == ' ' || ch == '\0' {
                continue;
            }
            let cx = self.padding + usize_to_u32(x) * self.metrics.width;
            self.draw_glyph(img, ch, cx, cy, rc);
        }
    }

    fn paint_bg_runs(
        &self,
        img: &mut RgbImage,
        row: &[Option<ResolvedCell<'_>>],
        snap_bg: HexColor,
        cy: u32,
    ) {
        let cols = usize_to_u32(row.len());
        let mut x = 0u32;
        while x < cols {
            let Some(rc) = &row[x as usize] else {
                x += 1;
                continue;
            };
            if rc.bg == snap_bg {
                x += 1;
                continue;
            }
            let run_bg = rc.bg;
            let mut x_end = x + 1;
            while x_end < cols {
                let Some(next) = &row[x_end as usize] else {
                    break;
                };
                if next.bg != run_bg {
                    break;
                }
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

    #[allow(clippy::cast_precision_loss)]
    fn draw_glyph(&self, img: &mut RgbImage, ch: char, cx: u32, cy: u32, rc: &ResolvedCell<'_>) {
        // Dim: blend 60% toward bg for the foreground.
        let fg = if rc.cell.is_dim() {
            mix(rc.fg, rc.bg, 0.6)
        } else {
            rc.fg
        };
        let glyph = self.font.glyph_id(ch).with_scale_and_position(
            self.scale,
            ab_glyph::point(cx as f32, (cy + self.metrics.baseline) as f32),
        );
        let Some(outlined) = self.font.outline_glyph(glyph) else {
            return;
        };
        let bounds = outlined.px_bounds();
        let (img_w, img_h) = (img.width(), img.height());
        outlined.draw(|gx, gy, coverage| {
            let Some(px) = pixel_coord(bounds.min.x, gx, img_w) else {
                return;
            };
            let Some(py) = pixel_coord(bounds.min.y, gy, img_h) else {
                return;
            };
            // Composite: linearly blend fg over the existing pixel by coverage.
            let existing = img.get_pixel(px, py);
            let blended = blend(*existing, Rgb([fg.r(), fg.g(), fg.b()]), coverage);
            img.put_pixel(px, py, blended);
        });
    }
}

/// Convert a glyph subpixel offset (`base: f32` + `delta: u32`) to an in-bounds
/// image coordinate. Returns `None` if the subpixel falls outside the image.
#[allow(clippy::cast_possible_truncation)]
fn pixel_coord(base: f32, delta: u32, max: u32) -> Option<u32> {
    let combined = base.round() as i64 + i64::from(delta);
    if combined < 0 {
        return None;
    }
    let v = u32::try_from(combined).ok()?;
    if v >= max { None } else { Some(v) }
}

/// Convert a `usize` (terminal coordinate, bounded in practice by ~256) to
/// `u32` for image arithmetic. Saturates to `u32::MAX` on overflow rather
/// than panicking — overflow is impossible with sane terminal sizes.
fn usize_to_u32(x: usize) -> u32 {
    u32::try_from(x).unwrap_or(u32::MAX)
}

/// Round an `f32` to the nearest non-negative `u32`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f32_round_to_u32(f: f32) -> u32 {
    f.round().max(0.0) as u32
}

/// Floor an `f32` to a non-negative `u32` (truncating toward zero).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f32_floor_to_u32(f: f32) -> u32 {
    f.max(0.0) as u32
}

#[derive(Debug, Clone, Copy)]
struct ResolvedCell<'a> {
    cell: &'a Cell,
    fg: HexColor,
    bg: HexColor,
}

/// Per-frame O(1) palette index lookup. Populated once from the
/// frame's [`PaletteOverrides`], then queried for every cell that
/// references an indexed palette color. Replaces the linear scan
/// inside `PaletteOverrides::get` on the per-cell hot path.
struct PaletteLookup {
    table: [Option<HexColor>; 256],
    is_empty: bool,
}

impl PaletteLookup {
    fn build(overrides: &PaletteOverrides) -> Self {
        let mut table = [None; 256];
        let is_empty = overrides.is_empty();
        if !is_empty {
            for (idx, color) in overrides.iter() {
                table[idx as usize] = Some(color);
            }
        }
        Self { table, is_empty }
    }

    #[inline]
    fn get(&self, idx: u8) -> Option<HexColor> {
        if self.is_empty {
            return None;
        }
        self.table[idx as usize]
    }
}

fn resolve<'a>(cell: &'a Cell, snap: &Frame, palette: &PaletteLookup) -> ResolvedCell<'a> {
    let mut fg = resolve_color(&cell.fg, snap.fg, palette);
    let mut bg = resolve_color(&cell.bg, snap.bg, palette);
    if cell.is_inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    ResolvedCell { cell, fg, bg }
}

/// Per-frame variant of [`CellColor::resolve`] that consults the
/// precomputed [`PaletteLookup`] instead of the original
/// [`PaletteOverrides`]'s linear scan.
fn resolve_color(
    color: &CellColor,
    default_for_layer: HexColor,
    palette: &PaletteLookup,
) -> HexColor {
    match color {
        CellColor::Default => default_for_layer,
        CellColor::Rgb(c) => *c,
        CellColor::Palette { idx, fallback } => {
            if let Some(fb) = fallback {
                return *fb;
            }
            if let Some(over) = palette.get(*idx) {
                return over;
            }
            if (*idx as usize) < crate::color::DEFAULT_ANSI_16.len() {
                return crate::color::DEFAULT_ANSI_16[*idx as usize];
            }
            default_for_layer
        }
        // `CellColor` is `#[non_exhaustive]`; future variants fall
        // back to the layer default rather than panicking.
        #[allow(unreachable_patterns, clippy::match_same_arms)]
        _ => default_for_layer,
    }
}

fn fill_rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, c: HexColor) {
    let px = Rgb([c.r(), c.g(), c.b()]);
    let (w, h) = (img.width(), img.height());
    // Clamp both corners so an origin past the image edge is a no-op
    // instead of a panic on `put_pixel`.
    let x0 = x0.min(w);
    let y0 = y0.min(h);
    let x1 = x1.min(w);
    let y1 = y1.min(h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            img.put_pixel(x, y, px);
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn blend(under: Rgb<u8>, over: Rgb<u8>, alpha: f32) -> Rgb<u8> {
    let a = alpha.clamp(0.0, 1.0);
    let lerp = |u: u8, o: u8| (f32::from(u) * (1.0 - a) + f32::from(o) * a).round() as u8;
    Rgb([
        lerp(under.0[0], over.0[0]),
        lerp(under.0[1], over.0[1]),
        lerp(under.0[2], over.0[2]),
    ])
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn mix(a: HexColor, b: HexColor, t: f32) -> HexColor {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    HexColor::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{CellColor, PaletteOverrides};
    use crate::frame::{Cell, Grid};

    fn cell(ch: char, fg: CellColor, bg: CellColor) -> Cell {
        Cell {
            ch: ch.to_string(),
            fg,
            bg,
            bold: 0,
            dim: 0,
            italic: 0,
            underline: 0,
            inverse: 0,
        }
    }

    #[test]
    fn font_loads() {
        Painter::new(FONT_BYTES, PaintConfig::default()).expect("font loads");
    }

    #[test]
    fn dims_match_grid() {
        let p = Painter::new(FONT_BYTES, PaintConfig::default()).unwrap();
        let snap = Frame {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![
                vec![
                    Some(cell(
                        'a',
                        CellColor::Default,
                        CellColor::Default
                    ));
                    80
                ];
                30
            ]),
        };
        let m = p.metrics();
        let (w, h) = p.image_dims(&snap);
        assert_eq!(w, 80 * m.width + 24);
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
    fn fill_rect_origin_past_bounds_is_noop() {
        // An x0/y0 past the image edge must not panic; the rect is empty.
        let mut img = RgbImage::new(4, 4);
        fill_rect(&mut img, 10, 10, 20, 20, HexColor::from_rgb(255, 0, 0));
        fill_rect(&mut img, 0, 10, 4, 20, HexColor::from_rgb(255, 0, 0));
        fill_rect(&mut img, 10, 0, 20, 4, HexColor::from_rgb(255, 0, 0));
        // Reversed/empty rects also no-op.
        fill_rect(&mut img, 3, 3, 1, 1, HexColor::from_rgb(255, 0, 0));
        assert_eq!(img.get_pixel(0, 0), &Rgb([0, 0, 0]));
        assert_eq!(img.get_pixel(3, 3), &Rgb([0, 0, 0]));
    }

    #[test]
    fn paint_produces_image_with_correct_dims() {
        let p = Painter::new(FONT_BYTES, PaintConfig::default()).unwrap();
        let snap = Frame {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![vec![
                Some(cell(
                    'h',
                    CellColor::Default,
                    CellColor::Default
                ));
                5
            ]]),
        };
        let img = p.paint(&snap);
        let (w, h) = p.image_dims(&snap);
        assert_eq!(img.width(), w);
        assert_eq!(img.height(), h);
    }

    #[test]
    fn save_png_error_carries_path() {
        // Save to a non-existent directory — must fail with the path in
        // the error chain so parallel-rendering failures are debuggable.
        let p = Painter::new(FONT_BYTES, PaintConfig::default()).unwrap();
        let snap = Frame {
            bg: HexColor::from_rgb(0, 0, 0),
            fg: HexColor::from_rgb(255, 255, 255),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![vec![Some(cell(
                'x',
                CellColor::Default,
                CellColor::Default,
            ))]]),
        };
        let bad = std::path::Path::new("/no/such/dir/out.png");
        let err = p.save_png(&snap, bad).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("/no/such/dir/out.png"),
            "error chain missing path: {chain}",
        );
    }

    #[test]
    fn paint_is_byte_stable() {
        // Two separate paints of identical input must produce identical bytes.
        let p = Painter::new(FONT_BYTES, PaintConfig::default()).unwrap();
        let snap = Frame {
            bg: HexColor::from_rgb(0x1a, 0x1b, 0x26),
            fg: HexColor::from_rgb(0xc0, 0xca, 0xf5),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![vec![
                Some(cell('h', CellColor::Default, CellColor::Default)),
                Some(cell('i', CellColor::Default, CellColor::Default)),
            ]]),
        };
        let a = p.paint(&snap);
        let b = p.paint(&snap);
        assert_eq!(a.as_raw(), b.as_raw());
    }
}
