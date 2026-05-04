.PHONY: setup build build-image recorder-warm recorder-warm-reset render demo demo-fast demo-parallel demo-all demo-all-fast demo-all-parallel demo-readme demo-web smoke picker picker-timeline-prototype cli cd-hook custom-theme recorder-perf bench-tiny bench-churn bench-subloops bench-subloops-parallel bench all-scenes verify verify-all clean

SCENE     ?= demo_full
CAST       = assets/$(SCENE).cast
OUT_EXT   ?= .gif
OUT        = assets/$(SCENE)$(OUT_EXT)
IMAGE     := tint-recorder:demo
WARM_CONTAINER ?= tint-recorder-warm
TINT_PATH ?= /home/cory/code/tint/tint
# Painter font size in pixels. Cell width and height scale linearly.
# Default 14 → 7×16 cells → 80×20 grid renders at 584×344 (good for dev
# loops; smaller files; not crisp on HiDPI). Marketing targets bump this
# in the demo-readme / demo-web targets below.
FONT_SIZE ?= 14

# Host requirements: cargo (build scene/render binaries), docker (recording),
# node/npm (xterm snapshot replay), and ffmpeg (encoding).
setup:
	@command -v cargo  >/dev/null && echo "cargo:  $$(cargo --version)"  || (echo "missing cargo"  && exit 1)
	@command -v docker >/dev/null && echo "docker: $$(docker --version)" || (echo "missing docker" && exit 1)
	@command -v node   >/dev/null && echo "node:   $$(node --version)"   || (echo "missing node"   && exit 1)
	@command -v npm    >/dev/null && echo "npm:    $$(npm --version)"    || (echo "missing npm"    && exit 1)
	@command -v ffmpeg >/dev/null && echo "ffmpeg: $$(ffmpeg -version | head -1)" || (echo "missing ffmpeg" && exit 1)

# Compile every host-side scene/render binary. Scene binaries drive the
# container via PTY; snapshot, paint, encode, and verify run on the host.
build:
	cargo build --release --bin smoke --bin demo_full \
	            --bin picker --bin picker_timeline --bin cli --bin cd_hook --bin custom_theme \
	            --bin bench_tiny --bin bench_churn --bin bench_subloops \
	            --bin paint --bin encode --bin verify --bin stitch \
	            --bin recorder_perf --bin compare_snapshots

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

demo: demo-parallel
demo-fast: demo-parallel

# Fast demo render. The recorder now uses content-aware sync points, so a
# single warm container is faster than four parallel docker starts plus stitch.
#
# Override OUT_EXT and FONT_SIZE for high-res variants:
#   make demo-parallel                       # default GIF, FONT_SIZE=14
#   make demo-parallel OUT_EXT=.mp4 FONT_SIZE=28
demo-parallel: build recorder-warm
	@echo "=== fast record: warm content-aware recorder ==="
	TINT_RECORDER_CONTAINER=$(WARM_CONTAINER) ./target/release/demo_full --cast assets/demo_full.cast
	@echo "=== render ==="
	rm -rf assets/snapshots assets/frames
	./node_modules/.bin/tsx ./renderer/snapshot.ts assets/demo_full.cast assets/snapshots
	./target/release/paint --font-size $(FONT_SIZE) assets/snapshots assets/frames
	./target/release/encode assets/frames assets/snapshots/timing.json assets/demo_full$(OUT_EXT)
	./target/release/verify demo_full --snapshots-dir assets/snapshots
	@echo "wrote assets/demo_full$(OUT_EXT)"

# Render BOTH the MP4 (for the website) and the GIF (for the README)
# from a single set of paint frames. Paint at FONT_SIZE=28 once
# (1144x624), then encode MP4 native + GIF scaled-down to width=824
# in parallel. Saves duplicate paint work and lets the two encoders
# share CPU cores via parallel ffmpeg invocations.
#
# Full marketing render flow:
#   make demo-all-parallel  →  fast record + paint + parallel encode
demo-all: demo-all-parallel
demo-all-fast: demo-all-parallel

demo-all-parallel: build recorder-warm
	@echo "=== fast record: warm content-aware recorder ==="
	TINT_RECORDER_CONTAINER=$(WARM_CONTAINER) ./target/release/demo_full --cast assets/demo_full.cast
	@echo "=== paint at FONT_SIZE=28 ==="
	rm -rf assets/snapshots assets/frames
	./node_modules/.bin/tsx ./renderer/snapshot.ts assets/demo_full.cast assets/snapshots
	./target/release/paint --font-size 28 assets/snapshots assets/frames
	@echo "=== parallel encode: mp4 + scaled gif ==="
	./target/release/encode assets/frames assets/snapshots/timing.json assets/demo_full.mp4 & \
	./target/release/encode assets/frames assets/snapshots/timing.json assets/demo_full.gif --width 824 & \
	wait
	./target/release/verify demo_full --snapshots-dir assets/snapshots
	@echo "wrote assets/demo_full.mp4 + assets/demo_full.gif"

# Marketing-quality renders. Both use FONT_SIZE bumps so cell metrics
# scale up proportionally. Dimensions are 80 cols × 20 rows × cell.
#
# - demo-readme: GIF at FONT_SIZE=20 → ~824×464. Fits the GitHub
#   README content area (~924px) without scaling, stays under the
#   per-image size budget, and is crisp at 1× DPR.
# - demo-web:    MP4 at FONT_SIZE=28 → ~1144×624 (≈2× the dev default).
#   MP4's compression handles this resolution at <500KB; the same
#   content as a GIF would be several MB. Crisp on HiDPI displays.
demo-readme: SCENE=demo_full
demo-readme: FONT_SIZE=20
demo-readme: OUT_EXT=.gif
demo-readme: build build-image render

demo-web: SCENE=demo_full
demo-web: FONT_SIZE=28
demo-web: OUT_EXT=.mp4
demo-web: build build-image render

smoke: SCENE=smoke
smoke: build build-image render

# Per-feature scenes (each gets its own GIF + verify contract).
picker: SCENE=picker
picker: build build-image render

picker-timeline-prototype: build build-image
	@echo "=== record picker semantic trace ==="
	./target/release/picker_timeline \
	    --cast assets/picker_timeline.cast \
	    --trace assets/picker_timeline.trace.json
	@echo "=== snapshot + paint ==="
	rm -rf assets/picker_timeline_snapshots assets/picker_timeline_frames
	./node_modules/.bin/tsx ./renderer/snapshot.ts assets/picker_timeline.cast assets/picker_timeline_snapshots
	./target/release/paint --font-size 28 assets/picker_timeline_snapshots assets/picker_timeline_frames
	@echo "=== encode: CPU paint + libx264, CPU paint + NVENC, GIF ==="
	./target/release/encode assets/picker_timeline_frames assets/picker_timeline_snapshots/timing.json assets/picker_timeline_libx264.mp4 --mp4-encoder libx264
	./target/release/encode assets/picker_timeline_frames assets/picker_timeline_snapshots/timing.json assets/picker_timeline_nvenc.mp4 --mp4-encoder h264_nvenc
	./target/release/encode assets/picker_timeline_frames assets/picker_timeline_snapshots/timing.json assets/picker_timeline.gif --width 824
	./target/release/verify picker --snapshots-dir assets/picker_timeline_snapshots
	@echo "wrote assets/picker_timeline_libx264.mp4 + assets/picker_timeline_nvenc.mp4 + assets/picker_timeline.gif"

cli: SCENE=cli
cli: build build-image render

cd-hook: SCENE=cd_hook
cd-hook: build build-image render

custom-theme: SCENE=custom_theme
custom-theme: build build-image render

recorder-perf: build recorder-warm
	TINT_RECORDER_CONTAINER=$(WARM_CONTAINER) ./target/release/recorder_perf --iterations 5

# Benchmark scenes for measuring pipeline performance. All use the
# default FONT_SIZE so timings reflect the dev-loop render path.
#
# - bench-tiny: ~3s of cast time, isolates fixed pipeline overhead
#   (tsx startup, paint init, ffmpeg cold-start, docker run setup).
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
bench-tiny: build build-image render

bench-churn: SCENE=bench_churn
bench-churn: build build-image render

bench-subloops: SCENE=bench_subloops
bench-subloops: build build-image render

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
	./target/release/stitch \
	    --out assets/bench_subloops.cast \
	    assets/bench_subloops_0.cast \
	    assets/bench_subloops_1.cast \
	    assets/bench_subloops_2.cast \
	    assets/bench_subloops_3.cast
	@echo "=== render ==="
	rm -rf assets/snapshots assets/frames
	./node_modules/.bin/tsx ./renderer/snapshot.ts assets/bench_subloops.cast assets/snapshots
	./target/release/paint --font-size $(FONT_SIZE) assets/snapshots assets/frames
	./target/release/encode assets/frames assets/snapshots/timing.json assets/bench_subloops.gif
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
# entirely on the host: tsx + target/release/ binaries + ffmpeg are
# all available without docker, so eliminating the second container
# saves the docker run startup overhead per render.
render:
	./target/release/$(SCENE) --cast $(CAST)
	rm -rf assets/snapshots assets/frames
	./node_modules/.bin/tsx ./renderer/snapshot.ts $(CAST) assets/snapshots
	./target/release/paint --font-size $(FONT_SIZE) assets/snapshots assets/frames
	./target/release/encode assets/frames assets/snapshots/timing.json $(OUT)
	./target/release/verify $(SCENE) --snapshots-dir assets/snapshots
	@echo "wrote $(OUT)"

# Manual verify (rerun against existing snapshots).
verify:
	docker run --rm -v $(CURDIR)/assets:/work $(IMAGE) \
		tint-verify $(SCENE) --snapshots-dir /work/snapshots

# Render every registered scene and report PASS/FAIL per scene. Drives
# the scene list from `tint-verify --list-scenes` so it stays in sync
# with the contract registry. Slow — runs the full render pipeline for
# each scene. Returns non-zero if any scene fails verify.
verify-all: build build-image
	@scenes=$$(docker run --rm $(IMAGE) tint-verify --list-scenes); \
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

clean:
	rm -rf assets/snapshots assets/frames assets/*_snapshots assets/*_frames assets/concat.txt
	rm -f assets/*.cast assets/*.gif assets/*.mp4 assets/*.trace.json
