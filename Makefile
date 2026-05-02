.PHONY: setup build-image demo smoke clean

PY        := .venv/bin/python
SCENE     ?= demo_full
CAST      := assets/$(SCENE).cast
GIF       := assets/$(SCENE).gif
IMAGE     := tint-recorder:demo
TINT_PATH ?= /home/cory/code/tint/tint

setup:
	python3 -m venv .venv
	$(PY) -m pip install -r requirements.txt
	npm install

# Build the Docker image used as the demo's bash environment. Must rerun
# whenever the host's tint script changes (or when Dockerfile changes).
# Build context is a tar stream (Dockerfile + tint), avoiding any temp dir.
build-image:
	tar -c Dockerfile -C $(dir $(TINT_PATH)) $(notdir $(TINT_PATH)) | \
		docker build -t $(IMAGE) -

demo: SCENE=demo_full
demo: build-image render

smoke: SCENE=smoke
smoke: build-image render

render:
	$(PY) -m scenes.$(SCENE)
	node renderer/snapshot.js $(CAST) assets/snapshots
	$(PY) renderer/paint.py assets/snapshots assets/frames
	$(PY) renderer/encode.py assets/frames assets/snapshots/timing.json $(GIF)
	@echo "wrote $(GIF)"

clean:
	rm -rf assets/snapshots assets/frames assets/concat.txt
	rm -f assets/*.cast assets/*.gif
