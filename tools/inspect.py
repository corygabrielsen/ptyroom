"""Inspect a single snapshot JSON: ASCII-render the grid with optional color.

Usage:
  python -m tools.inspect <snapshot-or-cast.json> [--color] [--rows R1:R2]
  python -m tools.inspect assets/snapshots/0060.json
  python -m tools.inspect assets/snapshots/0060.json --color
  python -m tools.inspect assets/snapshots/0060.json --rows 0:5    # head
  python -m tools.inspect assets/snapshots/0060.json --rows 25:    # tail

Output is line-numbered so it's easy to ask "what's on row 30?".
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


# Same xterm palette as paint.py's DEFAULT_ANSI — used as a last-resort
# fallback when a cell uses a palette index 0-15 with no OSC 4 override
# in the snapshot. Inspect rendering is best-effort; paint.py is the
# canonical resolver for the real GIF.
DEFAULT_ANSI = [
    "#000000", "#cd0000", "#00cd00", "#cdcd00",
    "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
    "#7f7f7f", "#ff0000", "#00ff00", "#ffff00",
    "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
]


def hex_to_rgb(s: str | None) -> tuple[int, int, int] | None:
    if not s or not isinstance(s, str) or not s.startswith("#"):
        return None
    s = s.lstrip("#")
    return int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16)


def cell_color(cell: dict, default_hex: str, key: str,
               palette: dict[str, str]) -> str | None:
    """Resolve a cell's bg or fg color spec to a hex string for ANSI output.

    Mirrors paint.py's resolve_color: palette refs fall back to the cell's
    inline fallback, then to the snapshot's OSC 4 palette dict, then to
    DEFAULT_ANSI for indices 0-15.
    """
    c = cell.get(key)
    if c is None:
        return default_hex
    if isinstance(c, str):
        return c
    idx = c.get("palette")
    fb = c.get("fallback") or palette.get(str(idx))
    if fb:
        return fb
    if isinstance(idx, int) and 0 <= idx < 16:
        return DEFAULT_ANSI[idx]
    return None


def render_row(row: list, default_bg: str, default_fg: str,
               palette: dict[str, str], color: bool) -> str:
    out: list[str] = []
    for cell in row:
        if cell is None:
            out.append(" ")
            continue
        ch = cell.get("ch") or " "
        if not color:
            out.append(ch)
            continue
        bg = cell_color(cell, default_bg, "bg", palette)
        fg = cell_color(cell, default_fg, "fg", palette)
        bg_rgb = hex_to_rgb(bg)
        fg_rgb = hex_to_rgb(fg)
        seq = ""
        if bg_rgb:
            seq += f"\x1b[48;2;{bg_rgb[0]};{bg_rgb[1]};{bg_rgb[2]}m"
        if fg_rgb:
            seq += f"\x1b[38;2;{fg_rgb[0]};{fg_rgb[1]};{fg_rgb[2]}m"
        out.append(seq + ch)
    if color:
        out.append("\x1b[0m")
    return "".join(out)


def parse_rows(spec: str, total: int) -> tuple[int, int]:
    """`5:10` → (5, 10). `5:` → (5, total). `:10` → (0, 10)."""
    if ":" not in spec:
        n = int(spec)
        return n, n + 1
    a, b = spec.split(":", 1)
    return (int(a) if a else 0, int(b) if b else total)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("snapshot")
    ap.add_argument("--color", action="store_true",
                    help="Render with ANSI true-color (bg + fg)")
    ap.add_argument("--rows", default=":",
                    help="Row slice as start:end (default all)")
    args = ap.parse_args()

    with Path(args.snapshot).open() as f:
        snap = json.load(f)

    grid = snap["grid"]
    bg = snap.get("bg", "#000000")
    fg = snap.get("fg", "#ffffff")
    palette = snap.get("palette", {})
    total = len(grid)
    start, end = parse_rows(args.rows, total)

    print(f"{args.snapshot}: bg={bg} fg={fg} {len(grid)}x{len(grid[0])}",
          file=sys.stderr)
    width = len(str(total))
    for i in range(start, end):
        line = render_row(grid[i], bg, fg, palette, args.color)
        print(f"{i+1:>{width}}  {line}")


if __name__ == "__main__":
    main()
