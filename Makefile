.PHONY: setup build build-image recorder-warm recorder-warm-reset render \
        all demo-walkthrough demo-features \
        smoke picker picker-timeline-prototype cli cd-hook custom-theme \
        recorder-perf bench-tiny bench-churn bench-subloops bench-subloops-parallel bench \
        all-scenes verify verify-all bless-goldens verify-goldens characterize \
        lint lint-fix clean

.DEFAULT_GOAL := all

SCENE     ?= demo_full
CAST       = assets/$(SCENE).cast
OUT_EXT   ?= .gif
OUT        = assets/$(SCENE)$(OUT_EXT)
IMAGE     := tint-recorder:demo
WARM_CONTAINER ?= tint-recorder-warm
TINT_PATH ?= ../tint/tint
# Export so scene binaries pick TINT_PATH up via clap's `env=` attribute
# without each recipe having to mention it. Without this, demo_full's
# host-side lookup_picker_idx falls back to its CLI default (a dead
# absolute path on the original author's machine) and fails with
# "No such file or directory".
export TINT_PATH
FEATURE_SCENES := cli picker cd_hook custom_theme
# Painter font size in pixels. Cell width and height scale linearly.
# Default 14 → 7×16 cells → 80×20 grid renders at 584×344 (good for dev
# loops; smaller files; not crisp on HiDPI). The marketing demo-walkthrough
# and demo-features targets pin FONT_SIZE=40 internally for crisp output.
FONT_SIZE ?= 14

# Host requirements: cargo (build everything), docker (recording),
# ffmpeg (encoding), and pre-commit + cargo-sort + cargo-machete (lint).
# Snapshot replay runs in-process via the vt100 crate — no Node/npm.
setup:
	@command -v cargo         >/dev/null && echo "cargo:         $$(cargo --version)"         || (echo "missing cargo"         && exit 1)
	@command -v docker        >/dev/null && echo "docker:        $$(docker --version)"        || (echo "missing docker"        && exit 1)
	@command -v ffmpeg        >/dev/null && echo "ffmpeg:        $$(ffmpeg -version | head -1)" || (echo "missing ffmpeg"        && exit 1)
	@command -v pre-commit    >/dev/null && echo "pre-commit:    $$(pre-commit --version)"    || (echo "missing pre-commit (pip install pre-commit)" && exit 1)
	@command -v cargo-sort    >/dev/null && echo "cargo-sort:    $$(cargo-sort --version)"    || (echo "missing cargo-sort (cargo install cargo-sort)"       && exit 1)
	@command -v cargo-machete >/dev/null && echo "cargo-machete: $$(cargo-machete --version)" || (echo "missing cargo-machete (cargo install cargo-machete)" && exit 1)
	@pre-commit install

# Compile every host-side binary across both workspace crates:
# - tint-recorder: generic encode/paint/stitch/inspect/compare_snapshots
#   /stress-child binaries.
# - tint-recorder-scenes: tint-coupled scene drivers + verify +
#   pipeline-test + recorder_perf.
build:
	cargo build --workspace --release

# Build the recording-only image. Just Dockerfile + the tint script —
# everything post-recording (snapshot replay, paint, encode, verify)
# runs on the host. Build context is a tar stream — no temp dir.
build-image:
	tar -c Dockerfile -C $(dir $(TINT_PATH)) $(notdir $(TINT_PATH)) | \
		docker build -t $(IMAGE) -

recorder-warm: build-image
	@if [ "$$(docker inspect -f '{{.State.Running}}' $(WARM_CONTAINER) 2>/dev/null)" = "true" ]; then \
		echo "warm recorder: $(WARM_CONTAINER)"; \
	else \
		docker rm -f $(WARM_CONTAINER) >/dev/null 2>&1 || true; \
		docker run -d --name $(WARM_CONTAINER) $(IMAGE) sleep infinity >/dev/null; \
		echo "started warm recorder: $(WARM_CONTAINER)"; \
	fi

recorder-warm-reset: build-image
	docker rm -f $(WARM_CONTAINER) >/dev/null 2>&1 || true
	docker run -d --name $(WARM_CONTAINER) $(IMAGE) sleep infinity >/dev/null
	@echo "started warm recorder: $(WARM_CONTAINER)"

# Render every demo: the composite walkthrough + each per-feature demo.
# Sequential because both targets drive the same warm recorder container.
all: demo-walkthrough demo-features

# Composite walkthrough demo (cli + cd_hook + picker + custom_theme in one cast).
# Paint at FONT_SIZE=40 once (~1704×864 Retina-crisp); encode MP4 native +
# GIF scaled to width 824 in parallel via `& wait`.
demo-walkthrough: build recorder-warm
	@echo "=== record demo_full ==="
	TERM_RECORDER_CONTAINER=$(WARM_CONTAINER) ./target/release/demo_full --cast assets/demo_full.cast
	@echo "=== paint at FONT_SIZE=40 ==="
	rm -rf assets/snapshots assets/frames
	./target/release/term-recorder snapshot assets/demo_full.cast assets/snapshots
	./target/release/term-recorder paint --font-size 40 assets/snapshots assets/frames
	@echo "=== parallel encode: mp4 native + scaled gif ==="
	./target/release/term-recorder encode assets/frames assets/snapshots/timing.json assets/demo_full.mp4 & \
	./target/release/term-recorder encode assets/frames assets/snapshots/timing.json assets/demo_full.gif --width 824 & \
	wait
	./target/release/verify demo_full --snapshots-dir assets/snapshots
	@echo "wrote assets/demo_full.mp4 + assets/demo_full.gif"

# Per-feature demos: one cast per feature (cli, picker, cd_hook,
# custom_theme). Scenes run in parallel against the shared warm
# container; the recorder gives each scene a unique container $HOME
# via the CONTAINER_HOME_SEQ atomic counter + pid, and each scene's
# disk paths (assets/<scene>_*) are disjoint.
demo-features: build recorder-warm
	@printf '%s\n' $(FEATURE_SCENES) | \
	    xargs -P $(words $(FEATURE_SCENES)) -I{} \
	    ./target/release/pipeline-test render {}

smoke: SCENE=smoke
smoke: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

# Per-feature scenes (each gets its own GIF + verify contract).
picker: SCENE=picker
picker: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

picker-timeline-prototype: build build-image
	@echo "=== record picker semantic trace ==="
	./target/release/picker_timeline \
	    --cast assets/picker_timeline.cast \
	    --trace assets/picker_timeline.trace.json
	@echo "=== snapshot + paint ==="
	rm -rf assets/picker_timeline_snapshots assets/picker_timeline_frames
	./target/release/term-recorder snapshot assets/picker_timeline.cast assets/picker_timeline_snapshots
	./target/release/term-recorder paint --font-size 28 assets/picker_timeline_snapshots assets/picker_timeline_frames
	@echo "=== encode: CPU paint + libx264, CPU paint + NVENC, GIF ==="
	./target/release/term-recorder encode assets/picker_timeline_frames assets/picker_timeline_snapshots/timing.json assets/picker_timeline_libx264.mp4 --mp4-encoder libx264
	./target/release/term-recorder encode assets/picker_timeline_frames assets/picker_timeline_snapshots/timing.json assets/picker_timeline_nvenc.mp4 --mp4-encoder h264_nvenc
	./target/release/term-recorder encode assets/picker_timeline_frames assets/picker_timeline_snapshots/timing.json assets/picker_timeline.gif --width 824
	./target/release/verify picker --snapshots-dir assets/picker_timeline_snapshots
	@echo "wrote assets/picker_timeline_libx264.mp4 + assets/picker_timeline_nvenc.mp4 + assets/picker_timeline.gif"

cli: SCENE=cli
cli: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

cd-hook: SCENE=cd_hook
cd-hook: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

custom-theme: SCENE=custom_theme
custom-theme: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

recorder-perf: build recorder-warm
	TERM_RECORDER_CONTAINER=$(WARM_CONTAINER) ./target/release/recorder_perf --iterations 5

# Benchmark scenes for measuring pipeline performance. All use the
# default FONT_SIZE so timings reflect the dev-loop render path.
#
# - bench-tiny: ~3s of cast time, isolates fixed pipeline overhead
#   (snapshot replay init, paint init, ffmpeg cold-start, docker run setup).
# - bench-churn: ~12s of rapid theme cycling, stresses per-frame work
#   (palette diversity for GIF, inter-frame deltas for MP4).
# - bench-subloops: 4 uniform subloops sequentially. Sequential
#   baseline for the future parallelize-and-stitch refactor of the
#   demo_full subloop pattern. Same shape as `make demo` but with
#   uniform synthetic content so subloop count is the only variable.
#
# Use `time make bench-tiny` / `time make bench-churn` /
# `time make bench-subloops` to see how each stage scales. `make
# bench` runs all three back-to-back.
bench-tiny: SCENE=bench_tiny
bench-tiny: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

bench-churn: SCENE=bench_churn
bench-churn: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

bench-subloops: SCENE=bench_subloops
bench-subloops: build build-image
	$(MAKE) render SCENE=$(SCENE) FONT_SIZE=$(FONT_SIZE) OUT_EXT=$(OUT_EXT)

# Parallel-record variant: spawn N copies of bench_subloops in
# parallel, each recording one subloop into its own cast, then
# stitch the casts and run the normal post-recording pipeline. Each
# instance gets its own docker container, so wall-time is bounded by
# the slowest single-subloop record (~5s) instead of N times that.
#
# This is the proof-of-concept for the same parallelization applied
# to demo_full. Today bench-subloops takes ~40s; bench-subloops-parallel
# should land closer to ~10s.
bench-subloops-parallel: build build-image
	@echo "=== parallel record: 4 subloops ==="
	@printf '0\n1\n2\n3\n' | \
		xargs -P 4 -I{} ./target/release/bench_subloops \
		    --subloop-only {} \
		    --cast assets/bench_subloops_{}.cast
	@echo "=== stitch ==="
	./target/release/term-recorder stitch \
	    --out assets/bench_subloops.cast \
	    assets/bench_subloops_0.cast \
	    assets/bench_subloops_1.cast \
	    assets/bench_subloops_2.cast \
	    assets/bench_subloops_3.cast
	@echo "=== render ==="
	rm -rf assets/snapshots assets/frames
	./target/release/term-recorder snapshot assets/bench_subloops.cast assets/snapshots
	./target/release/term-recorder paint --font-size $(FONT_SIZE) assets/snapshots assets/frames
	./target/release/term-recorder encode assets/frames assets/snapshots/timing.json assets/bench_subloops.gif
	./target/release/verify bench_subloops --snapshots-dir assets/snapshots
	@echo "wrote assets/bench_subloops.gif"

bench: bench-tiny bench-churn bench-subloops

# Build everything once, then render every scene against the same image.
all-scenes: build build-image
	$(MAKE) render SCENE=picker
	$(MAKE) render SCENE=cli
	$(MAKE) render SCENE=cd_hook
	$(MAKE) render SCENE=custom_theme
	$(MAKE) render SCENE=demo_full

# Two phases. Recording stage runs the scene binary on the host, which
# drives docker for the bash session and writes the cast to assets/.
# Post-recording (snapshot replay → paint → encode → verify) runs
# entirely on the host as Rust binaries + ffmpeg, so eliminating the
# second container saves the docker run startup overhead per render.
render:
	./target/release/$(SCENE) --cast $(CAST)
	rm -rf assets/snapshots assets/frames
	./target/release/term-recorder snapshot $(CAST) assets/snapshots
	./target/release/term-recorder paint --font-size $(FONT_SIZE) assets/snapshots assets/frames
	./target/release/term-recorder encode assets/frames assets/snapshots/timing.json $(OUT)
	./target/release/verify $(SCENE) --snapshots-dir assets/snapshots
	@echo "wrote $(OUT)"

# Manual verify (rerun against existing snapshots).
verify: build
	./target/release/verify $(SCENE) --snapshots-dir assets/snapshots

# Render every registered scene and report PASS/FAIL per scene. Drives
# the scene list from `tint-verify --list-scenes` so it stays in sync
# with the contract registry. Slow — runs the full render pipeline for
# each scene. Returns non-zero if any scene fails verify.
verify-all: build build-image
	@scenes=$$(./target/release/verify --list-scenes); \
	failed=""; \
	for scene in $$scenes; do \
		printf '\n=== %s ===\n' "$$scene"; \
		out=$$($(MAKE) -s render SCENE=$$scene 2>&1 || true); \
		printf '%s\n' "$$out" | grep -E '^(PASS|FAIL|wrote )' || true; \
		if printf '%s' "$$out" | grep -q '^FAIL'; then \
			failed="$$failed $$scene"; \
		fi; \
	done; \
	if [ -n "$$failed" ]; then \
		printf '\nFAILED:%s\n' "$$failed"; exit 1; \
	else \
		printf '\nall scenes passed\n'; \
	fi

# Run the pipeline N=10 times per scene (`pipeline-test bless --runs`);
# refuse to write a golden if any layer disagrees across runs (the
# safety net against goldening non-deterministic output). On success,
# writes `goldens/<scene>.json`. Override BLESS_RUNS=... or pass extra
# `--scenes=foo,bar` flags via PIPELINE_TEST_FLAGS.
bless-goldens: build recorder-warm
	./target/release/pipeline-test bless $(if $(BLESS_RUNS),--runs $(BLESS_RUNS),) $(PIPELINE_TEST_FLAGS)

# Run the pipeline once per scene, compare each layer hash against the
# committed `goldens/<scene>.json`, print PASS/FAIL per layer. Exits
# non-zero on any FAIL or missing golden. Override via PIPELINE_TEST_FLAGS.
verify-goldens: build recorder-warm
	./target/release/pipeline-test verify $(PIPELINE_TEST_FLAGS)

# Run each scene N=RUNS times (default 3); aggregate distinct hashes
# per layer into a STABLE/VARIES report at target/characterize/report.md.
characterize: build recorder-warm
	./target/release/pipeline-test characterize $(if $(RUNS),--runs $(RUNS),) $(PIPELINE_TEST_FLAGS)

# Run every pre-commit hook against every file. Identical to what
# `git commit` triggers, so a green `make lint` is sufficient to
# pass the hook on the next commit.
lint:
	pre-commit run --all-files

# Apply auto-fixable lint mutations. cargo-machete has no auto-fix
# (unused deps must be removed manually after review).
lint-fix:
	cargo fmt --all
	cargo sort --workspace
	cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

clean:
	rm -rf assets/snapshots assets/frames assets/*_snapshots assets/*_frames
	rm -f assets/*.cast assets/*.gif assets/*.mp4 assets/*.trace.json
