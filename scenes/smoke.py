"""Smoke test: drop into bash, type `tint` to launch picker, scroll, dismiss.

Run from project root:  python -m scenes.smoke
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recorder.driver import Recorder


def main():
    r = Recorder(cols=100, rows=30)
    r.start()

    r.dwell(800, settle_ms=600)              # let bash prompt appear
    r.type_text("tint", per_char_ms=80)      # launch picker
    r.key("enter", dwell_ms=400)
    r.dwell(800)                             # picker renders

    r.key("down", dwell_ms=120, repeat=5)
    r.dwell(500)

    r.key("up", dwell_ms=120, repeat=2)
    r.dwell(500)

    r.key("escape", dwell_ms=400)
    r.dwell(600)

    r.stop()

    out = r.write_cast("assets/smoke.cast")
    print(f"wrote {out} ({out.stat().st_size} bytes, {r.event_count()} events)")


if __name__ == "__main__":
    main()
