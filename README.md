# term-recorder

Deterministic GIF/MP4 recorder for scripted terminal demos. Spawns any interactive process under a PTY, captures raw IO, emits an asciinema cast whose timestamps come from virtual presentation time, then renders cast → PNG frames → GIF/MP4.

The recorder library is process-agnostic. The example consumer in this workspace targets [tint](https://github.com/corygabrielsen/tint).

## Workspace layout

Two crates:

- `term-recorder` (this directory) — generic recorder library (PTY driver, drainer, OSC stub, cast/snapshot/paint/encode/verify primitives). No domain coupling. Generic CLI binaries: `encode`, `paint`, `stitch`, `compare_snapshots`, `inspect`, `stress-child`.
- `tint-scenes/` — tint-specific scene helpers, contract registry, pipeline orchestration. Scene binaries (`cli`, `picker`, `cd_hook`, `custom_theme`, `demo_full`, `smoke`, `picker_timeline`, `bench_*`), `verify`, `pipeline-test`, `recorder_perf`. Depends on `term-recorder`.

The crate boundary is the architectural seam: nothing in `term-recorder/src/` imports anything domain-specific. Reusing the recorder against another interactive process is a `term-recorder = { path = ... }` dependency away.

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
JSON, concatenated PNGs, mp4, gif, plus snapshot/png counts). Make
targets driven by the `pipeline-test` binary:

```bash
make verify-goldens     # one pass per scene; PASS/FAIL per layer
make bless-goldens      # re-bake goldens; refuses if N=10 runs disagree
make characterize       # report determinism per layer per scene
```

The bless agreement gate is the safety net against goldening
non-determinism: if any layer's hash differs across the N=10 verify
runs the bless aborts. Override `BLESS_RUNS=...` to tune the floor;
pass `PIPELINE_TEST_FLAGS='--scenes=foo,bar'` for subset operation.

Recorder library timing primitives are exercised in
`tests/recorder_stress.rs` against a synthetic generic child
(`src/bin/stress_child.rs`). Tests assert the wait_for cutoff
contract directly and verify byte-stability under parallel load
and CPU contention. **Architectural rule:** these tests import
`term_recorder::*` only — never any consumer crate. The recorder
library is meant to be domain-generic; the seam is enforced by
where tests draw their dependencies.

## Determinism

- Recording shell runs in a pinned `debian:12-slim` image with a fresh `$HOME`. No host `$PATH` leakage.
- PTY winsize is fixed before exec.
- The driver answers OSC 10/11 color queries with canned RGB, so the recorded process runs unmodified.
- Cast timestamps come from cumulative `dwell_ms`, never wall clock.
- `wait_for` cuts off the captured event at the pattern's end byte; bytes that arrive after the pattern stay in the drainer buffer for the next operation. Without this cutoff a slow recorder-thread wake under contention would scoop up post-pattern bytes that on a fast wake would belong to the next event — producing partition drift in the cast.
- libx264 mp4 encoding is pinned to `-threads 1` and the concat demuxer's manifest is written to a per-call tempfile, so encodes are byte-stable across runs and concurrent encodes against the same frame set don't race.
- Glyph rasterization uses a bundled font (`include_bytes!`).
- Raw IO, diagnostic wall time, and playback time are separate layers in the trace.

## Authoring scenes

Scenes are small Rust binaries (in a consumer crate) that drive a `Recorder`. Use `Recorder::spawn` for an arbitrary local process; use `Recorder::start` for a Docker-backed shell session. Prefer content-aware gates (`send_raw_wait_for`, plus consumer-defined helpers like `wait_for_prompt`, `ps2_enter`) over fixed sleeps and bare `Key::Enter` — the recorder's default settle is microseconds and not a substitute for syncing on a known byte pattern. Use presentation helpers only for output that does not affect shell state (comments, blank prompt lines, clear boundaries).

Working examples live in `examples/` and the `tint-scenes/scenes/` consumer crate.

## Why a TypeScript shim

`@xterm/headless` is the only mature terminal emulator with proper OSC 11 support. Everything else is Rust.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera license; see `assets/fonts/LICENSE-DejaVu.txt`.
