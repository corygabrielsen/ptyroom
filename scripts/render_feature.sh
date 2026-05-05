#!/usr/bin/env bash
#
# Render one feature scene end-to-end (record → snapshot → paint →
# encode → verify) against the shared warm Docker container.
#
# Designed to be safe to run in parallel with other instances:
# - Recorder gives each invocation a unique container $HOME via its
#   atomic counter + pid (src/recorder/mod.rs CONTAINER_HOME_SEQ).
# - Each scene has its own asset prefix (assets/<scene>.cast,
#   assets/<scene>_snapshots, assets/<scene>_frames, assets/<scene>.mp4,
#   assets/<scene>.gif), so disk paths don't collide.
# - encode tempfiles are per-call (src/encode.rs).
#
# Usage: scripts/render_feature.sh <scene>
# Env:   WARM_CONTAINER  docker exec target (default: tint-recorder-warm)
#        FONT_SIZE       paint --font-size  (default: 40)
#        WIDTH           gif --width        (default: 824)
# Run from tint-recorder/ root.

set -euo pipefail

scene="${1:?usage: scripts/render_feature.sh <scene>}"
container="${WARM_CONTAINER:-tint-recorder-warm}"
font_size="${FONT_SIZE:-40}"
width="${WIDTH:-824}"

cast="assets/${scene}.cast"
snaps="assets/${scene}_snapshots"
frames="assets/${scene}_frames"
mp4="assets/${scene}.mp4"
gif="assets/${scene}.gif"

rm -rf "$snaps" "$frames"

echo "=== record $scene ==="
TINT_RECORDER_CONTAINER="$container" "./target/release/${scene}" --cast "$cast"

echo "=== snapshot + paint $scene at FONT_SIZE=$font_size ==="
./node_modules/.bin/tsx ./renderer/snapshot.ts "$cast" "$snaps"
./target/release/paint --font-size "$font_size" "$snaps" "$frames"

echo "=== encode $scene: mp4 + gif ==="
./target/release/encode "$frames" "$snaps/timing.json" "$mp4" &
./target/release/encode "$frames" "$snaps/timing.json" "$gif" --width "$width" &
wait

./target/release/verify "$scene" --snapshots-dir "$snaps"
echo "wrote $mp4 + $gif"
