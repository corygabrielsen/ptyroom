#!/bin/bash
# Render an asciinema cast to a GIF inside the demo container.
#
# Usage: render-cast.sh <cast-path> <gif-path>
# Intermediate snapshots/ and frames/ dirs land beside the cast.
set -euo pipefail
CAST="$1"
GIF="$2"
DIR=$(dirname "$CAST")

# Call tsx directly (skip the npx resolution dance, ~100-300ms cold start).
/app/node_modules/.bin/tsx /app/renderer/snapshot.ts "$CAST" "$DIR/snapshots"
python3 /app/renderer/paint.py "$DIR/snapshots" "$DIR/frames"
python3 /app/renderer/encode.py "$DIR/frames" "$DIR/snapshots/timing.json" "$GIF"
