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
make demo-features      # per-feature demos (one each, recorded in parallel)
make verify             # re-run contract against existing snapshots
```

See `Makefile` for the full target list.

## Regression gate

The pipeline pins nine layer hashes per scene under `goldens/<scene>.json`
(concat-of-output-bytes, cast event count, final + concatenated snapshot
JSON, concatenated PNGs, mp4, gif, plus snapshot/png counts). Two
make targets:

```bash
make verify-goldens     # one pass per scene; PASS/FAIL per layer
make bless-goldens      # re-bake goldens; refuses if N=10 runs disagree
```

The bless agreement gate is the safety net against goldening
non-determinism: if any layer's hash differs across the N=10 verify
runs the bless aborts. Override `SCENES=...` for subset operation,
`BLESS_RUNS=...` to tune the floor.

Recorder library timing primitives are exercised in
`tests/recorder_stress.rs` against a synthetic non-tint child
(`tests/fixtures/stress_child.sh`). Tests assert the wait_for
cutoff contract directly and verify byte-stability under parallel
load and CPU contention. **Architectural rule:** these tests import
`tint_recorder::recorder` and `cast` only — never `scenes`. The
recorder library is meant to be domain-generic; the seam is enforced
by where tests draw their dependencies.

## Determinism

- Recording shell runs in a pinned `debian:12-slim` image with a fresh `$HOME`. No host `$PATH` / `.tint` leakage.
- PTY winsize is fixed before exec.
- The driver answers OSC 10/11 color queries with canned RGB, so the recorded process runs unmodified.
- Cast timestamps come from cumulative `dwell_ms`, never wall clock.
- `wait_for` cuts off the captured event at the pattern's end byte; bytes that arrive after the pattern stay in the drainer buffer for the next operation. Without this cutoff a slow recorder-thread wake under contention would scoop up post-pattern bytes that on a fast wake would belong to the next event — producing partition drift in the cast.
- libx264 mp4 encoding is pinned to `-threads 1` and the concat demuxer's manifest is written to a per-call tempfile, so encodes are byte-stable across runs and concurrent encodes against the same frame set don't race.
- Glyph rasterization uses a bundled font (`include_bytes!`).
- Raw IO, diagnostic wall time, and playback time are separate layers in the trace.

## Authoring scenes

Scenes are small Rust binaries under `scenes/` that drive a `Recorder`. Use `Recorder::spawn` for an arbitrary local process; use `Recorder::start` for the Docker-backed tint demo shell. Prefer content-aware gates (`wait_for_prompt`, `ps2_enter`, `send_raw_wait_for`) over fixed sleeps and bare `Key::Enter` — the recorder's default settle is microseconds and not a substitute for syncing on a known byte pattern. For heredoc body lines specifically, use `ps2_enter`, which waits on `\x1b[?2004h> ` (bracketed-paste-on + PS2). Use virtual presentation helpers only for output that does not affect shell state (comments, blank prompt lines, clear boundaries).

Working examples live in `examples/` and `scenes/`.

## Why a TypeScript shim

`@xterm/headless` is the only mature terminal emulator with proper OSC 11 support. Everything else is Rust.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera license; see `assets/fonts/LICENSE-DejaVu.txt`.
