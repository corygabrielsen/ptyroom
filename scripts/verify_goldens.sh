#!/usr/bin/env bash
#
# Run the pipeline once per scene, compare each layer's hash against the
# committed golden, and report PASS/FAIL per layer. Exit 0 if all PASS,
# 1 if any FAIL or any golden is missing.
#
# Usage:  scripts/verify_goldens.sh
# Env:    SCENES        space-separated scene list
#                       (default: cli picker cd_hook custom_theme demo_full)
#         GOLDEN_DIR    input dir (default: goldens)
#
# Run from tint-recorder/ root.

set -euo pipefail

SCENES="${SCENES:-cli picker cd_hook custom_theme demo_full}"
GOLDEN_DIR="${GOLDEN_DIR:-goldens}"
export TINT_PATH="${TINT_PATH:-../tint/tint}"

source "$(dirname "$0")/lib/pipeline.sh"

make build >/dev/null
make recorder-warm >/dev/null

LAYERS=(concat_o cast_event_count final_snapshot all_snapshots
        snapshot_count all_pngs png_count mp4 gif)

failed=0
for scene in $SCENES; do
  golden="$GOLDEN_DIR/${scene}.json"
  if [[ ! -f "$golden" ]]; then
    printf "FAIL  %s: no golden at %s\n" "$scene" "$golden"
    failed=1
    continue
  fi

  pipeline_run_scene "$scene"
  current=$(pipeline_hash_scene "$scene")

  for layer in "${LAYERS[@]}"; do
    g=$(jq -r ".hashes.${layer}" "$golden")
    c=$(jq -r ".${layer}" <<< "$current")
    if [[ "$g" == "$c" ]]; then
      printf "PASS  %s/%s\n" "$scene" "$layer"
    else
      printf "FAIL  %s/%s\n      golden=%s\n      current=%s\n" \
        "$scene" "$layer" "$g" "$c"
      failed=1
    fi
  done
done

exit "$failed"
