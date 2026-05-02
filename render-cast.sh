#!/bin/bash
# Render an asciinema cast to a GIF inside the demo container.
#
# Usage: render-cast.sh <scene-name> <cast-path> <gif-path>
#
# Intermediate snapshots/ and frames/ dirs land beside the cast.
# After rendering, runs the verify contract for `scene-name` and exits
# non-zero if any check fails.
set -euo pipefail
SCENE="$1"
CAST="$2"
GIF="$3"
DIR=$(dirname "$CAST")

# Hermetic intermediates: nuke the per-render dirs so leftover frames from
# a previous scene can't leak into this scene's verify (e.g. an earlier
# render with more events leaves higher-numbered snapshots that look like
# this scene's "final" frame).
rm -rf "$DIR/snapshots" "$DIR/frames"

# Snapshot replay still uses @xterm/headless via the JS file. That step
# stays in TypeScript because Rust has no equivalent terminal emulator
# with proper OSC 11 support (avt silently drops OSC).
/app/node_modules/.bin/tsx /app/renderer/snapshot.ts "$CAST" "$DIR/snapshots"
tint-paint  "$DIR/snapshots" "$DIR/frames"
tint-encode "$DIR/frames" "$DIR/snapshots/timing.json" "$GIF"
tint-verify "$SCENE" --snapshots-dir "$DIR/snapshots"
