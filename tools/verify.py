"""Verify a recorded scene against per-scene assertion contracts.

Usage:
  python -m tools.verify <scene-name>
  python -m tools.verify demo_full

Contracts live in `scene_contracts.py` next to this file. Each contract is
a list of named assertions evaluated against the snapshots/ directory the
scene wrote. Output is one line per check:

  PASS  scene/check-name           detail
  FAIL  scene/check-name           detail

Exit code 0 if all pass, 1 if any fail. Run as `make verify`.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from tools.scene_contracts import CONTRACTS
from tools.types import Snapshot

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SNAPSHOT_DIR = PROJECT_ROOT / "assets" / "snapshots"


def load_snapshots(scene: str) -> list[Snapshot]:
    """Load all numbered JSON snapshots written by the named scene's render."""
    paths = sorted(SNAPSHOT_DIR.glob("[0-9]*.json"))
    if not paths:
        raise SystemExit(
            f"no snapshots in {SNAPSHOT_DIR}; run `make {scene}` first"
        )
    snaps = []
    for p in paths:
        with p.open() as f:
            d = json.load(f)
        snaps.append(Snapshot(
            idx=int(p.stem),
            bg=d.get("bg", "#000000"),
            fg=d.get("fg", "#ffffff"),
            grid=d["grid"],
        ))
    return snaps


def run(scene: str) -> int:
    contract = CONTRACTS.get(scene)
    if not contract:
        raise SystemExit(f"no contract defined for scene {scene!r}")
    snaps = load_snapshots(scene)
    failed = 0
    for check in contract:
        ok, detail = check.fn(snaps)
        marker = "PASS" if ok else "FAIL"
        print(f"{marker}  {scene}/{check.name:32}  {detail}")
        if not ok:
            failed += 1
    return 0 if failed == 0 else 1


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("scene")
    args = ap.parse_args()
    sys.exit(run(args.scene))


if __name__ == "__main__":
    main()
