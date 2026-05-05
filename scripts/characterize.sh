#!/usr/bin/env bash
#
# Determinism characterization (Layer 0 of the regression-guardrail design).
#
# For each scene in $SCENES, run the full pipeline $RUNS times and hash
# each layer per run. Aggregate distinct values per layer per scene and
# report which layers are observed-stable vs varies.
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
# Layers measured per run:
#   concat_o            sha256 of concat(event.data | event.kind="o")
#   cast_event_count    int
#   final_snapshot      sha256 of last <scene>_snapshots/NNNN.json
#   all_snapshots       sha256 of cat(all snapshot json in frame order)
#   snapshot_count      int
#   all_pngs            sha256 of cat(all png in frame order)
#   png_count           int
#   mp4                 sha256
#   gif                 sha256
#
# Run from tint-recorder/ root.

set -euo pipefail

SCENES="${SCENES:-cli picker cd_hook custom_theme demo_full}"
RUNS="${RUNS:-3}"
RESET="${RESET:-0}"
WIDTH=824
FONT_SIZE=40
WARM_CONTAINER="${WARM_CONTAINER:-tint-recorder-warm}"
export TINT_PATH="${TINT_PATH:-../tint/tint}"

OUT=target/characterize
mkdir -p "$OUT"

scene_to_binary() {
  case "$1" in
    cli|picker|cd_hook|custom_theme|demo_full) echo "$1" ;;
    *) echo "unknown scene: $1" >&2; exit 1 ;;
  esac
}

run_pipeline() {
  local scene="$1"
  local binary; binary=$(scene_to_binary "$scene")
  local cast="assets/${scene}.cast"
  local snaps="assets/${scene}_snapshots"
  local frames="assets/${scene}_frames"
  local mp4="assets/${scene}.mp4"
  local gif="assets/${scene}.gif"

  rm -rf "$snaps" "$frames"

  TINT_RECORDER_CONTAINER="$WARM_CONTAINER" \
    "./target/release/${binary}" --cast "$cast" >/dev/null

  ./node_modules/.bin/tsx ./renderer/snapshot.ts "$cast" "$snaps" >/dev/null

  ./target/release/paint --font-size "$FONT_SIZE" "$snaps" "$frames" >/dev/null

  ./target/release/encode "$frames" "$snaps/timing.json" "$mp4" >/dev/null 2>&1
  ./target/release/encode "$frames" "$snaps/timing.json" "$gif" --width "$WIDTH" >/dev/null 2>&1
}

hashes_for() {
  local scene="$1"
  local cast="assets/${scene}.cast"
  local snaps="assets/${scene}_snapshots"
  local frames="assets/${scene}_frames"
  local mp4="assets/${scene}.mp4"
  local gif="assets/${scene}.gif"

  local concat_o cast_count final_snap all_snaps snap_count all_pngs png_count mp4_sha gif_sha
  concat_o=$(tail -n +2 "$cast" | jq -j 'select(.[1]=="o") | .[2]' | sha256sum | awk '{print $1}')
  cast_count=$(tail -n +2 "$cast" | grep -c .)

  final_snap=$(ls "$snaps"/[0-9]*.json | sort | tail -1 | xargs sha256sum | awk '{print $1}')
  all_snaps=$(ls "$snaps"/[0-9]*.json | sort | xargs cat | sha256sum | awk '{print $1}')
  snap_count=$(ls "$snaps"/[0-9]*.json | wc -l)

  all_pngs=$(ls "$frames"/[0-9]*.png | sort | xargs cat | sha256sum | awk '{print $1}')
  png_count=$(ls "$frames"/[0-9]*.png | wc -l)

  mp4_sha=$(sha256sum "$mp4" | awk '{print $1}')
  gif_sha=$(sha256sum "$gif" | awk '{print $1}')

  jq -nc \
    --arg concat_o "$concat_o" \
    --argjson cast_event_count "$cast_count" \
    --arg final_snapshot "$final_snap" \
    --arg all_snapshots "$all_snaps" \
    --argjson snapshot_count "$snap_count" \
    --arg all_pngs "$all_pngs" \
    --argjson png_count "$png_count" \
    --arg mp4 "$mp4_sha" \
    --arg gif "$gif_sha" \
    '{concat_o:$concat_o, cast_event_count:$cast_event_count,
      final_snapshot:$final_snapshot, all_snapshots:$all_snapshots,
      snapshot_count:$snapshot_count, all_pngs:$all_pngs,
      png_count:$png_count, mp4:$mp4, gif:$gif}'
}

# build everything once
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
    run_pipeline "$scene"
    hashes_for "$scene" >> "$out"
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
      # truncate long hashes
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
