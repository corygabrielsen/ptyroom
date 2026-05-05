# tint-recorder

Deterministic GIF/MP4 recorder for scripted terminal demos. Spawns an interactive process under a PTY, captures raw IO, emits an asciinema cast whose timestamps come from virtual presentation time, then renders cast → PNG frames → GIF/MP4.

Current scenes target [tint](https://github.com/corygabrielsen/tint). The recorder core is process-agnostic.

## Pipeline

```
scenes/<scene>.rs   → cast    PTY driver + scripted input + OSC responder
renderer/snapshot.ts → frames  @xterm/headless replay → per-frame JSON
src/paint.rs        → PNGs    JSON + bundled font → image
src/encode.rs       → GIF/MP4 ffmpeg concat-demuxer
src/verify.rs                 per-scene contract on rendered frames
```

## Setup

```bash
make setup
```

Requires `cargo`, `docker`, Node/npm, and `ffmpeg`.

## Run

```bash
make all                # render every demo
make demo-walkthrough   # composite walkthrough (cli + cd_hook + picker + custom_theme)
make demo-features      # per-feature demos (one each)
make verify             # re-run contract against existing snapshots
```

See `Makefile` for the full target list.

## Determinism

- Recording shell runs in a pinned `debian:12-slim` image with a fresh `$HOME`. No host `$PATH` / `.tint` leakage.
- PTY winsize is fixed before exec.
- The driver answers OSC 10/11 color queries with canned RGB, so the recorded process runs unmodified.
- Cast timestamps come from cumulative `dwell_ms`, never wall clock.
- Glyph rasterization uses a bundled font (`include_bytes!`).
- Raw IO, diagnostic wall time, and playback time are separate layers in the trace.

## Authoring scenes

Scenes are small Rust binaries under `scenes/` that drive a `Recorder`. Use `Recorder::spawn` for an arbitrary local process; use `Recorder::start` for the Docker-backed tint demo shell. Prefer content-aware gates (`wait_for_prompt`, `send_raw_wait_for`) over fixed sleeps. Use virtual presentation helpers only for output that does not affect shell state (comments, blank prompt lines, clear boundaries).

Working examples live in `examples/` and `scenes/`.

## Why a TypeScript shim

`@xterm/headless` is the only mature terminal emulator with proper OSC 11 support. Everything else is Rust.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera license; see `assets/fonts/LICENSE-DejaVu.txt`.
