"""Render snapshot JSON files to PNG frames.

Each snapshot has:
  - bg, fg: terminal-level colors (from OSC 11/10)
  - palette: ANSI palette overrides (from OSC 4)
  - grid: rows of cells with {ch, fg, bg, bold, dim, italic, ...}

Usage:
  python paint.py <snapshots-dir> <out-dir> [--font <ttf>] [--font-size 16]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


# Bundled font — pinned for cross-machine byte-stable GIFs. Anchored to
# the script's location so cwd doesn't matter.
BUNDLED_FONT = (
    Path(__file__).resolve().parent.parent
    / "assets" / "fonts" / "DejaVuSansMono.ttf"
)


# Default ANSI 16-color palette (xterm). Used as fallback when a cell uses
# a palette index AND no OSC 4 override exists for that index.
DEFAULT_ANSI = [
    "#000000", "#cd0000", "#00cd00", "#cdcd00",
    "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
    "#7f7f7f", "#ff0000", "#00ff00", "#ffff00",
    "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
]


def hex_to_rgb(s: str) -> tuple[int, int, int]:
    s = s.lstrip("#")
    return int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16)


def resolve_color(c, palette: dict, default_hex: str) -> tuple[int, int, int]:
    """Resolve a cell color spec to an RGB tuple.

    A cell's fg/bg can be:
      - None (use the default — caller's terminal fg or bg)
      - "#rrggbb" (truecolor)
      - {"palette": idx, "fallback": "#rrggbb" or null}
    """
    if c is None:
        return hex_to_rgb(default_hex)
    if isinstance(c, str):
        return hex_to_rgb(c)
    idx = c["palette"]
    # JSON serializes int dict keys as strings, so palette is keyed by str(idx)
    fb = c.get("fallback") or palette.get(str(idx))
    if fb:
        return hex_to_rgb(fb)
    if 0 <= idx < 16:
        return hex_to_rgb(DEFAULT_ANSI[idx])
    return hex_to_rgb(default_hex)


def render_snapshot(snap, font, cell_w, cell_h, padding) -> Image.Image:
    rows = len(snap["grid"])
    cols = len(snap["grid"][0])
    img_w = cols * cell_w + 2 * padding
    img_h = rows * cell_h + 2 * padding

    bg_rgb = hex_to_rgb(snap["bg"])
    img = Image.new("RGB", (img_w, img_h), bg_rgb)
    draw = ImageDraw.Draw(img)

    palette = snap.get("palette", {})

    for y, row in enumerate(snap["grid"]):
        cy = padding + y * cell_h

        # Pass 1: resolve every cell's effective fg/bg once.
        resolved = []
        for cell in row:
            if cell is None:
                resolved.append(None)
                continue
            cb = resolve_color(cell["bg"], palette, snap["bg"])
            cf = resolve_color(cell["fg"], palette, snap["fg"])
            if cell.get("inverse"):
                cb, cf = cf, cb
            resolved.append((cell, cb, cf))

        # Pass 2: paint backgrounds in runs of identical bg per row.
        # Coalesces ~100 per-cell rectangles into a handful of wide ones
        # for uniform-bg regions (highlighted picker rows, gradient strips).
        x = 0
        while x < cols:
            r = resolved[x]
            if r is None or r[1] == bg_rgb:
                x += 1
                continue
            run_bg = r[1]
            x_end = x + 1
            while x_end < cols:
                rn = resolved[x_end]
                if rn is None or rn[1] != run_bg:
                    break
                x_end += 1
            draw.rectangle(
                [padding + x * cell_w, cy,
                 padding + x_end * cell_w, cy + cell_h],
                fill=run_bg,
            )
            x = x_end

        # Pass 3: paint glyphs.
        for x, r in enumerate(resolved):
            if r is None:
                continue
            cell, cb, cf = r
            ch = cell["ch"]
            if not ch or ch == " ":
                continue
            if cell.get("dim"):
                fg = tuple(int(0.4 * cf[i] + 0.6 * cb[i]) for i in range(3))
            else:
                fg = cf
            draw.text((padding + x * cell_w, cy), ch, font=font, fill=fg)

    return img


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("snap_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--font", default=str(BUNDLED_FONT))
    ap.add_argument("--font-size", type=int, default=14)
    ap.add_argument("--padding", type=int, default=12)
    args = ap.parse_args()

    snap_dir = Path(args.snap_dir)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    font = ImageFont.truetype(args.font, args.font_size)

    # Measure cell size from the font
    bbox = font.getbbox("M")
    cell_w = bbox[2] - bbox[0]
    cell_h = font.size + 2

    snaps = sorted(p for p in snap_dir.glob("[0-9]*.json"))
    print(f"painting {len(snaps)} frames at cell {cell_w}x{cell_h}")
    for p in snaps:
        with p.open() as f:
            snap = json.load(f)
        img = render_snapshot(snap, font, cell_w, cell_h, args.padding)
        out_path = out_dir / f"{p.stem}.png"
        img.save(out_path)
    print(f"wrote PNGs to {out_dir}")


if __name__ == "__main__":
    main()
