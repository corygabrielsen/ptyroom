"""Per-scene visual assertion contracts.

Each scene maps to a list of `Check` objects. Each check receives the full
list of snapshots and returns `(passed, detail)`.

Conventions:
  - `expected_bg(snaps, idx, color)` — assert snapshot at idx has bg=color
  - `find_first_bg(snaps, color)` — find first snap with bg=color (returns idx or None)
  - `row_contains(snaps, idx, row, substring)` — assert row contains substring

Contracts are intentionally a few load-bearing checks per scene, not
exhaustive — they catch regressions, they don't validate every pixel.
"""

from __future__ import annotations

from tools.types import Check, Snapshot


# ───────── helpers ─────────

def find_first_bg(snaps: list[Snapshot], color: str) -> int | None:
    for s in snaps:
        if s.bg.lower() == color.lower():
            return s.idx
    return None


def find_picker_indicator(snaps: list[Snapshot]) -> tuple[int, int] | None:
    """Find the first (snap_idx, row_idx) where the picker scroll indicator
    `↓ N more` is visible — proves the picker isn't being cut off at the bottom."""
    for s in snaps:
        for r in range(s.rows):
            if "more" in s.row_text(r) and "↓" in s.row_text(r):
                return s.idx, r
    return None


# ───────── checks ─────────

def picker_scroll_indicator_visible() -> Check:
    """The picker should render its `↓ N more` scroll indicator on the bottom
    row at least once. If it never appears, the picker is being clipped."""
    def fn(snaps):
        hit = find_picker_indicator(snaps)
        if hit is None:
            return False, "picker scroll indicator (↓ N more) never visible"
        idx, row = hit
        return True, f"first seen at frame {idx:04d} row {row+1}"
    return Check("picker_scroll_indicator_visible", fn)


def bg_reaches(color: str, label: str) -> Check:
    """Some frame in the scene should have bg=color — proves the act for
    that theme actually fired."""
    def fn(snaps):
        idx = find_first_bg(snaps, color)
        if idx is None:
            return False, f"{label} ({color}) never applied"
        return True, f"{label} ({color}) reached at frame {idx:04d}"
    return Check(f"bg_reaches_{label}", fn)


def final_bg_is(color: str, label: str) -> Check:
    """The last snapshot's bg should be `color` — proves the demo lands
    on the expected theme at the end."""
    def fn(snaps):
        last = snaps[-1]
        if last.bg.lower() != color.lower():
            return False, f"final bg={last.bg}, expected {color} ({label})"
        return True, f"final bg={color} ({label})"
    return Check(f"final_bg_{label}", fn)


# ───────── per-scene contracts ─────────

CONTRACTS: dict[str, list[Check]] = {
    "demo_full": [
        picker_scroll_indicator_visible(),
        bg_reaches("#78593a", "dark-orange"),       # Act 1 picker landing
        bg_reaches("#282a36", "dracula"),           # Act 2 first CLI command
        bg_reaches("#fdf6e3", "solarized-light"),   # Act 2 second
        bg_reaches("#5353ac", "blue-cd-hook"),      # Act 3 first cd hook
        bg_reaches("#debacc", "pale-rose-cd-hook"), # Act 3 second cd hook
        bg_reaches("#ff006e", "hot-custom"),        # Act 4 custom .theme
        final_bg_is("#ff006e", "hot-custom"),       # outro lingers on Act 4
    ],
    "smoke": [
        picker_scroll_indicator_visible(),
    ],
}
