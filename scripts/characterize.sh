#!/usr/bin/env bash
#
# Determinism characterization (Layer 0 of the regression-guardrail design).
#
# For each scene in $SCENES, run the full pipeline $RUNS times and hash
# nine layers per run. Aggregate distinct values per layer per scene and
# report which are observed-stable vs which vary.
#
# Usage:   scripts/characterize.sh
# Env:     SCENES   space-separated scene names
#                   (default: cli picker cd_hook custom_theme demo_full)
#          RUNS     iterations per scene (default: 3)
#          RESET    1 = `recorder-warm-reset` between runs (default: 0,
#                   mirrors today's `make all` shared-warm-container path)
# Output:  target/characterize/<scene>.jsonl   one record per run
#          target/characterize/report.md       human-readable summary
#
# Run from tint-recorder/ root.

set -euo pipefail

SCENES="${SCENES:-cli picker cd_hook custom_theme demo_full}"
RUNS="${RUNS:-3}"
RESET="${RESET:-0}"
export TINT_PATH="${TINT_PATH:-../tint/tint}"

OUT=target/characterize
mkdir -p "$OUT"

source "$(dirname "$0")/lib/pipeline.sh"

make build >/dev/null
make recorder-warm >/dev/null

REPORT="$OUT/report.md"
{
  echo "# Determinism characterization"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Scenes:    $SCENES"
  echo "- Runs:      $RUNS"
  echo "- Reset between runs: $RESET"
  echo
} > "$REPORT"

for scene in $SCENES; do
  out="$OUT/${scene}.jsonl"
  : > "$out"
  echo "=== characterizing $scene (runs=$RUNS) ===" >&2
  for run in $(seq 0 $((RUNS - 1))); do
    if [[ "$RESET" == "1" ]]; then
      make recorder-warm-reset >/dev/null
    fi
    echo "  run $run" >&2
    pipeline_run_scene "$scene"
    pipeline_hash_scene "$scene" >> "$out"
  done

  {
    echo "## $scene"
    echo
    printf "| layer | status | distinct | sample |\n"
    printf "|---|---|---|---|\n"
    for layer in concat_o cast_event_count final_snapshot all_snapshots \
                 snapshot_count all_pngs png_count mp4 gif; do
      values=$(jq -r ".$layer" "$out" | tr '\n' ' ')
      distinct=$(echo $values | tr ' ' '\n' | grep -v '^$' | sort -u | wc -l)
      sample=$(echo $values | tr ' ' '\n' | grep -v '^$' | head -1)
      if [[ ${#sample} -gt 16 ]]; then sample="${sample:0:12}…"; fi
      if [[ "$distinct" == "1" ]]; then
        status="STABLE"
      else
        status="VARIES"
      fi
      printf "| %s | %s | %d | %s |\n" "$layer" "$status" "$distinct" "$sample"
    done
    echo
  } >> "$REPORT"
done

echo >&2
echo "report: $REPORT" >&2
echo "raw:    $OUT/<scene>.jsonl" >&2
