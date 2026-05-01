# tint-recorder

Deterministic GIF recorder for [tint](https://github.com/corygabrielsen/tint)
demos. Drives bash + tint through a PTY, captures every byte to an asciinema
cast, replays the cast through `@xterm/headless`, paints PNGs with Pillow,
and encodes a GIF with ffmpeg.

The pipeline is byte-stable: same scene, same machine, same GIF — pinned
font, hermetic env, cast timestamps derived from intent (`dwell_ms`) instead
of wall-clock.

## Pipeline

```
scenes/*.py    PTY → bash + tint, scripted keystrokes, byte capture
               → asciinema v2 .cast file (deterministic timestamps)

renderer/      snapshot.js → @xterm/headless replays cast → per-frame JSON
               paint.py    → Pillow renders each snapshot to PNG
               encode.py   → ffmpeg concat-demuxer → final GIF
```

## Setup

```bash
make setup
```

Requires `python3`, `node`, `npm`, and `ffmpeg` on PATH. Tested on
Linux (WSL).

## Run

```bash
make demo     # the polished 4-act marketing demo
make smoke    # minimal smoke test
make clean    # remove generated artifacts
```

Artifacts land in `assets/<scene>.gif`. Intermediate `snapshots/`,
`frames/`, and `*.cast` files are gitignored.

## Layout

| Path                   | Role                                               |
| ---------------------- | -------------------------------------------------- |
| `recorder/driver.py`   | PTY driver, byte capture, asciinema cast emitter   |
| `renderer/snapshot.js` | @xterm/headless replay → per-frame JSON            |
| `renderer/paint.py`    | Pillow renderer (PNG per frame, pinned font)       |
| `renderer/encode.py`   | ffmpeg concat-demuxer encoder                      |
| `scenes/`              | Recording scripts (one Python file per demo)       |
| `assets/fonts/`        | Bundled DejaVu Sans Mono (cross-machine stability) |

## Determinism

- Hermetic env (`env -i` style, explicit allow-list)
- PTY winsize fixed via `TIOCSWINSZ` before exec
- Tint's OSC 11/10 queries stubbed in the spawned bash
- Cast timestamps derived from cumulative `dwell_ms`, never wall-clock
- Bundled font ensures identical glyph rasterization across machines

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera
license; see `assets/fonts/LICENSE-DejaVu.txt`.
