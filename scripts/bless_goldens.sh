#!/usr/bin/env bash
#
# Bake current pipeline output as committed goldens.
#
# Runs the full pipeline $BLESS_RUNS times per scene. Refuses to write a
# golden if any layer disagrees across the runs — that's the safety net
# against goldening non-determinism. On agreement, writes
# goldens/<scene>.json with timestamp, run count, and layer hash dict.
#
# Usage:  scripts/bless_goldens.sh
# Env:    SCENES        space-separated scene list
#                       (default: cli picker cd_hook custom_theme demo_full)
#         BLESS_RUNS    agreement gate, must be ≥ 2 (default: 3)
#         GOLDEN_DIR    output dir (default: goldens)
#
# Run from tint-recorder/ root.

set -euo pipefail

SCENES="${SCENES:-cli picker cd_hook custom_theme demo_full}"
# 10 is the floor: 3 missed real races at this scale (e.g. picker
# parallel-event-loss). Higher N takes more wall-time but is the
# difference between a gate that protects the project and one that
# rubber-stamps subtle non-determinism.
BLESS_RUNS="${BLESS_RUNS:-10}"
GOLDEN_DIR="${GOLDEN_DIR:-goldens}"
export TINT_PATH="${TINT_PATH:-../tint/tint}"

if (( BLESS_RUNS < 2 )); then
  echo "BLESS_RUNS must be ≥ 2 (agreement gate); got $BLESS_RUNS" >&2
  exit 2
fi

source "$(dirname "$0")/lib/pipeline.sh"

mkdir -p "$GOLDEN_DIR"
make build >/dev/null
make recorder-warm >/dev/null

failed=0
for scene in $SCENES; do
  echo "=== blessing $scene (runs=$BLESS_RUNS) ===" >&2

  declare -a hashes_history=()
  for run in $(seq 0 $((BLESS_RUNS - 1))); do
    echo "  run $run" >&2
    pipeline_run_scene "$scene"
    hashes_history+=("$(pipeline_hash_scene "$scene")")
  done

  first="${hashes_history[0]}"
  ok=1
  for ((i = 1; i < ${#hashes_history[@]}; i++)); do
    if [[ "${hashes_history[i]}" != "$first" ]]; then
      ok=0
      break
    fi
  done

  if (( ok == 0 )); then
    echo "REFUSE bless $scene: layers disagreed across $BLESS_RUNS runs" >&2
    for ((i = 0; i < ${#hashes_history[@]}; i++)); do
      printf "  run %d: %s\n" "$i" "${hashes_history[i]}" >&2
    done
    failed=1
    unset hashes_history
    continue
  fi

  out="$GOLDEN_DIR/${scene}.json"
  jq -n \
    --arg scene "$scene" \
    --arg blessed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson blessed_runs "$BLESS_RUNS" \
    --argjson hashes "$first" \
    '{scene:$scene, blessed_at:$blessed_at, blessed_runs:$blessed_runs, hashes:$hashes}' \
    > "$out"
  echo "  wrote $out" >&2

  unset hashes_history
done

if (( failed != 0 )); then
  exit 1
fi
