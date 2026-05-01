"""Build a GIF from a PNG sequence + timing.json (per-frame dwells).

Uses ffmpeg's concat demuxer with `duration N.NNN` directives so each
frame holds for its specified dwell. No timing math, just declarative.

Usage:
  python encode.py <frames-dir> <timing.json> <out.gif>
"""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("frames_dir")
    ap.add_argument("timing_json")
    ap.add_argument("out_gif")
    ap.add_argument("--fps", type=int, default=25)
    args = ap.parse_args()

    frames_dir = Path(args.frames_dir).resolve()
    with open(args.timing_json) as f:
        timing = json.load(f)

    # Build ffmpeg concat-demuxer file
    concat_lines = []
    for entry in timing:
        png = frames_dir / f"{entry['frame']}.png"
        if not png.exists():
            raise FileNotFoundError(png)
        concat_lines.append(f"file '{png}'")
        concat_lines.append(f"duration {entry['dwell_ms']/1000:.4f}")
    # Concat demuxer requires last frame to be repeated (its duration is ignored)
    concat_lines.append(f"file '{frames_dir / (timing[-1]['frame'] + '.png')}'")

    concat_path = frames_dir.parent / "concat.txt"
    concat_path.write_text("\n".join(concat_lines) + "\n")

    # Run ffmpeg with palettegen + paletteuse for clean GIF colors
    # Two-pass via -filter_complex: split into two streams, palettegen one,
    # paletteuse the other.
    cmd = [
        "ffmpeg", "-y",
        "-f", "concat", "-safe", "0", "-i", str(concat_path),
        "-vf", f"fps={args.fps},split[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse=dither=bayer:bayer_scale=5",
        "-loop", "0",
        args.out_gif,
    ]
    print("$", " ".join(cmd))
    subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
