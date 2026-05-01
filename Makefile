.PHONY: setup demo smoke clean

PY := .venv/bin/python
SCENE ?= demo_full
CAST  := assets/$(SCENE).cast
GIF   := assets/$(SCENE).gif

setup:
	python3 -m venv .venv
	$(PY) -m pip install -r requirements.txt
	npm install

demo: SCENE=demo_full
demo: render

smoke: SCENE=smoke
smoke: render

render:
	$(PY) -m scenes.$(SCENE)
	node renderer/snapshot.js $(CAST) assets/snapshots
	$(PY) renderer/paint.py assets/snapshots assets/frames
	$(PY) renderer/encode.py assets/frames assets/snapshots/timing.json $(GIF)
	@echo "wrote $(GIF)"

clean:
	rm -rf assets/snapshots assets/frames assets/concat.txt
	rm -f assets/*.cast assets/*.gif
