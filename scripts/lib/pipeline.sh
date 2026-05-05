# Shared pipeline driver functions.
#
# Source from scripts/{characterize,bless_goldens,verify_goldens}.sh:
#     source "$(dirname "$0")/lib/pipeline.sh"
#
# Functions provided:
#   pipeline_scene_to_binary <scene>   # validate + echo binary name
#   pipeline_run_scene       <scene>   # full record→snapshot→paint→encode
#   pipeline_hash_scene      <scene>   # nine-layer JSON line of sha256/counts
#
# Configuration via env (with sensible defaults):
#   WIDTH            gif scale width      (default 824)
#   FONT_SIZE        paint cell pitch     (default 40)
#   WARM_CONTAINER   docker exec target   (default tint-recorder-warm)
#
# Caller is responsible for ensuring `make build` and `make recorder-warm`
# have been run, and for working from the tint-recorder/ root.

# shellcheck shell=bash

WIDTH="${WIDTH:-824}"
FONT_SIZE="${FONT_SIZE:-40}"
WARM_CONTAINER="${WARM_CONTAINER:-tint-recorder-warm}"

pipeline_scene_to_binary() {
  case "$1" in
    cli|picker|cd_hook|custom_theme|demo_full) echo "$1" ;;
    *) echo "unknown scene: $1" >&2; return 1 ;;
  esac
}

pipeline_run_scene() {
  local scene="$1"
  local binary; binary=$(pipeline_scene_to_binary "$scene") || return 1
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

pipeline_hash_scene() {
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
