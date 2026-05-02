.PHONY: setup build build-image render demo smoke picker cli cd-hook custom-theme all-scenes verify verify-all clean

SCENE     ?= demo_full
CAST       = assets/$(SCENE).cast
GIF        = assets/$(SCENE).gif
IMAGE     := tint-recorder:demo
TINT_PATH ?= /home/cory/code/tint/tint

# Host requirements: cargo (build scene binaries) + docker (run them).
setup:
	@command -v cargo  >/dev/null && echo "cargo:  $$(cargo --version)"  || (echo "missing cargo"  && exit 1)
	@command -v docker >/dev/null && echo "docker: $$(docker --version)" || (echo "missing docker" && exit 1)

# Compile every host-side scene binary. They drive the container via PTY;
# rendering happens inside the image.
build:
	cargo build --release --bin smoke --bin demo_full \
	            --bin picker --bin cli --bin cd_hook --bin custom_theme

# Build the demo image. Includes its own Rust builder stage so the
# in-container binaries (paint/encode/inspect/verify) match exactly.
# Build context is a tar stream — no temp dir.
build-image:
	tar -c Dockerfile render-cast.sh \
	       package.json package-lock.json tsconfig.json \
	       renderer src scenes assets Cargo.toml Cargo.lock \
	       -C $(dir $(TINT_PATH)) $(notdir $(TINT_PATH)) | \
		docker build -t $(IMAGE) -

demo: SCENE=demo_full
demo: build build-image render

smoke: SCENE=smoke
smoke: build build-image render

# Per-feature scenes (each gets its own GIF + verify contract).
picker: SCENE=picker
picker: build build-image render

cli: SCENE=cli
cli: build build-image render

cd-hook: SCENE=cd_hook
cd-hook: build build-image render

custom-theme: SCENE=custom_theme
custom-theme: build build-image render

# Build everything once, then render every scene against the same image.
all-scenes: build build-image
	$(MAKE) render SCENE=picker
	$(MAKE) render SCENE=cli
	$(MAKE) render SCENE=cd_hook
	$(MAKE) render SCENE=custom_theme
	$(MAKE) render SCENE=demo_full

# Two phases: scene binary on host (drives docker for the bash session,
# writes the cast to ./assets/), then docker mounts ./assets/ and runs
# render-cast.sh which calls tint-paint, tint-encode, tint-verify.
render:
	./target/release/$(SCENE) --cast $(CAST)
	docker run --rm -v $(CURDIR)/assets:/work $(IMAGE) \
		render-cast.sh $(SCENE) /work/$(SCENE).cast /work/$(SCENE).gif
	@echo "wrote $(GIF)"

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
	rm -rf assets/snapshots assets/frames assets/concat.txt
	rm -f assets/*.cast assets/*.gif
