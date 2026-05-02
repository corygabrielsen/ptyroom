.PHONY: setup build-image demo smoke verify clean

PY        := python3
SCENE     ?= demo_full
CAST      := assets/$(SCENE).cast
GIF       := assets/$(SCENE).gif
IMAGE     := tint-recorder:demo
TINT_PATH ?= /home/cory/code/tint/tint

# Host requirements: python3 (for the scene runner only — uses stdlib) and
# docker (everything else runs inside the container).
setup:
	@command -v python3 >/dev/null && echo "python3: $$(python3 --version)" || (echo "missing python3" && exit 1)
	@command -v docker  >/dev/null && echo "docker:  $$(docker --version)"  || (echo "missing docker"  && exit 1)

# Build the demo container (bash + tint + render pipeline). Rerun whenever
# the host's tint script, the Dockerfile, or render code changes.
build-image:
	tar -c Dockerfile render-cast.sh package.json package-lock.json \
	       requirements.txt renderer assets/fonts \
	       -C $(dir $(TINT_PATH)) $(notdir $(TINT_PATH)) | \
		docker build -t $(IMAGE) -

demo: SCENE=demo_full
demo: build-image render

smoke: SCENE=smoke
smoke: build-image render

# Two phases: scene runs on host (drives docker for the bash session,
# writes the cast to ./assets/), then render runs inside the container
# (consumes the cast, writes the GIF back to ./assets/).
render:
	$(PY) -m scenes.$(SCENE)
	docker run --rm -v $(CURDIR)/assets:/work $(IMAGE) \
		/app/render-cast.sh /work/$(SCENE).cast /work/$(SCENE).gif
	@echo "wrote $(GIF)"
	@$(MAKE) -s verify SCENE=$(SCENE)

# Verify a scene's recorded snapshots against its assertion contract.
# Run after `make demo` or `make smoke`. Exits non-zero on any failure.
verify:
	$(PY) -m tools.verify $(SCENE)

clean:
	rm -rf assets/snapshots assets/frames assets/concat.txt
	rm -f assets/*.cast assets/*.gif
