"""Shared types for the verification harness."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable


@dataclass
class Snapshot:
    idx: int
    bg: str
    fg: str
    grid: list[list[dict | None]]

    def row_text(self, row: int) -> str:
        cells = self.grid[row]
        return "".join((c.get("ch") or " ") if c else " " for c in cells).rstrip()

    @property
    def rows(self) -> int:
        return len(self.grid)

    @property
    def cols(self) -> int:
        return len(self.grid[0])


@dataclass
class Check:
    name: str
    fn: Callable[[list[Snapshot]], tuple[bool, str]]
