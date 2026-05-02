#!/bin/bash
# Render an asciinema cast to a GIF inside the demo container.
#
# Usage: render-cast.sh <cast-path> <gif-path>
# Intermediate snapshots/ and frames/ dirs land beside the cast.
set -euo pipefail
CAST="$1"
GIF="$2"
DIR=$(dirname "$CAST")

node /app/renderer/snapshot.js "$CAST" "$DIR/snapshots"
python3 /app/renderer/paint.py "$DIR/snapshots" "$DIR/frames"
python3 /app/renderer/encode.py "$DIR/frames" "$DIR/snapshots/timing.json" "$GIF"
