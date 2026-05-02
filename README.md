# tint-recorder

Deterministic GIF recorder for [tint](https://github.com/corygabrielsen/tint)
demos. Drives the real `tint` binary through a PTY inside a Docker container,
captures every byte to an asciinema cast, replays the cast through
`@xterm/headless`, paints PNGs with Pillow, and encodes a GIF with ffmpeg —
all inside the same pinned image.

Same scene, any host with Docker, the same GIF.

## Pipeline

```
recorder/driver.py    pty.fork → docker run → bash + tint, scripted
                      keystrokes, byte capture → asciinema v2 .cast
                      _PtyDrainer answers tint's OSC 11/10 queries
                      so real tint runs unmodified

render-cast.sh        in-container orchestrator:
  renderer/snapshot.js  @xterm/headless replays cast → per-frame JSON
  renderer/paint.py     Pillow renders each snapshot to PNG
  renderer/encode.py    ffmpeg concat-demuxer → final GIF
```

## Setup

```bash
make setup
```

Host requirements: **`python3` and `docker`**. Everything else (node,
ffmpeg, Pillow, the DejaVu font) lives inside the container.

## Run

```bash
make build-image  # build the demo container (one-time, then on tint changes)
make demo         # the polished 4-act marketing demo
make smoke        # minimal smoke test
make clean        # remove generated artifacts
```

`make demo` and `make smoke` depend on `build-image`, so a single `make
demo` builds the image if needed and produces the GIF. Artifacts land
in `assets/<scene>.gif`.

## Layout

| Path                   | Role                                              |
| ---------------------- | ------------------------------------------------- |
| `Dockerfile`           | Demo + render image (debian:12-slim base)         |
| `render-cast.sh`       | In-container cast → GIF orchestrator              |
| `recorder/driver.py`   | PTY driver, OSC responder, asciinema cast emitter |
| `renderer/snapshot.js` | @xterm/headless replay → per-frame JSON           |
| `renderer/paint.py`    | Pillow renderer (pinned DejaVu font)              |
| `renderer/encode.py`   | ffmpeg concat-demuxer encoder                     |
| `scenes/demo_full.py`  | 4-act marketing demo                              |
| `scenes/smoke.py`      | Minimal smoke scene                               |
| `assets/fonts/`        | Bundled DejaVu Sans Mono                          |

## Determinism

- Demo runs inside a pinned `debian:12-slim` image (no host `$HOME` /
  `$PATH` / `.tint` leakage)
- PTY winsize fixed via `TIOCSWINSZ` before exec
- Driver answers tint's OSC 11/10 color queries with canned RGB replies,
  so the real `tint` binary runs unmodified
- Cast timestamps derived from cumulative `dwell_ms`, never wall-clock
- Bundled font ensures identical glyph rasterization

## Authoring scenes

Scenes are Python scripts in `scenes/` that drive a `Recorder`:

```python
from recorder.driver import Recorder

r = Recorder(cols=80, rows=30)
r.start()
r.dwell(800, settle_ms=600)
r.type_text("tint dracula", per_char_ms=35)
r.key("enter", dwell_ms=400)
r.dwell(1200)
r.stop()
r.write_cast("assets/myscene.cast")
```

Scenes run on the host but spawn the bash session inside the container.
Every directory and file the demo references should be created on screen
during the recording (`mkdir`, `echo > .tint`, heredoc) — no magic
pre-prepared state.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera
license; see `assets/fonts/LICENSE-DejaVu.txt`.
