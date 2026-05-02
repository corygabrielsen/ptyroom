.PHONY: setup build build-image demo smoke verify clean

SCENE     ?= demo_full
CAST       = assets/$(SCENE).cast
GIF        = assets/$(SCENE).gif
IMAGE     := tint-recorder:demo
TINT_PATH ?= /home/cory/code/tint/tint

# Host requirements: cargo (build scene binaries) + docker (run them).
setup:
	@command -v cargo  >/dev/null && echo "cargo:  $$(cargo --version)"  || (echo "missing cargo"  && exit 1)
	@command -v docker >/dev/null && echo "docker: $$(docker --version)" || (echo "missing docker" && exit 1)

# Compile the host-side scene binaries (smoke, demo_full). They drive the
# container via PTY; rendering happens inside the image.
build:
	cargo build --release --bin smoke --bin demo_full

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

clean:
	rm -rf assets/snapshots assets/frames assets/concat.txt
	rm -f assets/*.cast assets/*.gif
