#!/bin/bash
# Render an asciinema cast to a GIF or MP4 inside the demo container.
#
# Usage: render-cast.sh <scene-name> <cast-path> <out-path>
#
# Output format is detected from <out-path>'s extension (.gif or .mp4).
# Intermediate snapshots/ and frames/ dirs land beside the cast.
# After rendering, runs the verify contract for `scene-name` and exits
# non-zero if any check fails.
#
# Env vars:
#   FONT_SIZE — pixel font size for the painter (default 14). Cell width
#               and height scale linearly. Higher = sharper at HiDPI but
#               larger files.
set -euo pipefail
SCENE="$1"
CAST="$2"
OUT="$3"
DIR=$(dirname "$CAST")
FONT_SIZE="${FONT_SIZE:-14}"

# Hermetic intermediates: nuke the per-render dirs so leftover frames from
# a previous scene can't leak into this scene's verify (e.g. an earlier
# render with more events leaves higher-numbered snapshots that look like
# this scene's "final" frame).
rm -rf "$DIR/snapshots" "$DIR/frames"

# Snapshot replay still uses @xterm/headless via the JS file. That step
# stays in TypeScript because Rust has no equivalent terminal emulator
# with proper OSC 11 support (avt silently drops OSC).
/app/node_modules/.bin/tsx /app/renderer/snapshot.ts "$CAST" "$DIR/snapshots"
tint-paint  --font-size "$FONT_SIZE" "$DIR/snapshots" "$DIR/frames"
tint-encode "$DIR/frames" "$DIR/snapshots/timing.json" "$OUT"
tint-verify "$SCENE" --snapshots-dir "$DIR/snapshots"
