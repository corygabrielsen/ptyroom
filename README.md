# term-recorder

Deterministic GIF/MP4 recorder for scripted terminal demos. Spawns any
interactive process under a PTY, captures raw IO, emits an asciinema
cast whose timestamps come from virtual presentation time, then renders
cast → PNG frames → GIF/MP4. Output is byte-stable across runs.

## Library use

Render an existing cast to media in one call:

```rust
term_recorder::render("demo.cast")?
    .font_size(40.0)
    .width(824)
    .to_path("demo.gif")?;
```

Output format is inferred from the path extension (`.mp4` or `.gif`).
Intermediate snapshot JSON and PNG frames live in a tempdir for the
duration of the call.

Drive an interactive process and produce a cast:

```rust
use std::time::Duration;
use term_recorder::recorder::{Recorder, RecorderConfig};

let mut rec = Recorder::spawn(RecorderConfig::default(), &["bash"])?;
rec.send_raw_wait_for(
    &[], Duration::ZERO,
    b"$ ", Duration::from_secs(2),
    "prompt",
)?;
rec.type_text("echo hello", Duration::from_millis(35))?;
rec.send_raw_wait_for(
    b"\n", Duration::from_millis(300),
    b"$ ", Duration::from_secs(2),
    "echo prompt",
)?;
rec.stop()?.write("hello.cast")?;
```

A working version is at `examples/generic_shell.rs`. The recorder
library is process-agnostic; the example consumer in this workspace
targets [tint](https://github.com/corygabrielsen/tint).

## Verifiable artifacts

Determinism isn't just an internal property — it's externally checkable
through two composable attestation layers:

**Receipts (provenance).** A receipt is a JSON sidecar that records
the cast hash, render config, tool / ffmpeg / font versions, and
output hash. Re-rendering on any machine with the same identity
should produce the same output bytes:

```rust
let receipt = term_recorder::render("demo.cast")?
    .font_size(40.0)
    .to_path_with_receipt("demo.gif")?;
receipt.write("demo.gif.receipt.json")?;
```

```bash
term-recorder verify --receipt demo.gif.receipt.json --cast demo.cast
# MATCH  →  exit 0
# CAST_DIFFERS / ENV_DIFFERS / OUTPUT_DIFFERS  →  exit 1
```

**Specs (behavior).** A spec is a JSON file listing predicates the
cast must satisfy. The verifier replays the cast and re-evaluates:

```json
{
  "version": 1,
  "predicates": [
    { "kind": "contains_text", "text": "$ tint dracula" },
    { "kind": "does_not_contain_text", "text": "error" }
  ]
}
```

```bash
term-recorder check --cast demo.cast --spec demo.spec.json
# PASS / FAIL per predicate;  exit 0 only if every predicate passes
```

**Composition.** A receipt can embed a `spec_sha256` so a single
`verify` covers both halves — provenance and behavior — at once:

```bash
term-recorder render demo.cast demo.gif \
    --receipt demo.gif.receipt.json \
    --spec    demo.spec.json
# receipt now carries spec_sha256

term-recorder verify --receipt demo.gif.receipt.json \
                     --cast    demo.cast \
                     --spec    demo.spec.json
# MATCH only if cast hash matches AND environment matches
# AND re-render output matches AND spec hash matches AND every
# predicate passes
```

The receipt format is nix-derivation-shaped (provenance + bit-exact
reproduction); the spec is in-toto-policy-shaped (behavioral
assertions). Both are pure deterministic functions of their inputs,
so on a chain that supports Rust execution they compose into a
single verifiable claim.

## CLI

One unified binary with subcommands:

```bash
term-recorder render <cast> <out> [--receipt R] [--spec S]   # cast → MP4/GIF (one call)
term-recorder verify --receipt R --cast C [--spec S]         # check a receipt
term-recorder check  --cast C --spec S                       # check a spec
term-recorder snapshot <cast> <out_dir>                      # cast → snapshot JSON
term-recorder paint <snap_dir> <out_dir>                     # snapshots → PNGs
term-recorder encode <frames> <timing> <out>                 # PNGs → MP4/GIF
term-recorder stitch --out OUT INPUT...                      # concatenate casts
term-recorder compare-snapshots <baseline> <candidate>       # frame-by-frame diff
term-recorder inspect <snapshot>                             # ASCII-render to terminal
```

`render` chains `snapshot → paint → encode` in memory; the lower
subcommands expose each stage separately when you want the intermediate
artifacts on disk. `verify` and `check` are the two attestation
verifiers (provenance and behavior).

## Pipeline

```
scenes/<scene>.rs       → cast    PTY driver + scripted input + OSC responder
src/snapshot_replay     → JSON    vt100 + OSC tracker → per-frame snapshots
src/paint.rs            → PNGs    JSON + bundled font → image
src/encode.rs           → GIF/MP4 ffmpeg concat-demuxer
src/verify.rs                     per-scene contract on rendered frames
```

## Workspace layout

Two crates:

- `term-recorder` (this directory) — generic recorder library + unified CLI binary. No domain coupling.
- `tint-scenes/` — tint-specific scene helpers, contract registry, pipeline orchestration. Scene binaries (`cli`, `picker`, `cd_hook`, `custom_theme`, `demo_full`, …), `verify`, `pipeline-test`, `recorder_perf`. Depends on `term-recorder`.

The crate boundary is the architectural seam: nothing in `term-recorder/src/` imports anything domain-specific. Reusing the recorder against another interactive process is a `term-recorder = { path = ... }` dependency away.

## Setup

```bash
make setup
```

Requires `cargo`, `docker`, and `ffmpeg`.

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
- PTY winsize is fixed before exec; `portable-pty` handles the platform-correct fork/exec/ctty dance.
- The driver answers OSC 10/11 color queries with canned RGB, so the recorded process runs unmodified.
- Cast timestamps come from cumulative `dwell_ms`, never wall clock.
- `wait_for` cuts off the captured event at the pattern's end byte; bytes that arrive after the pattern stay in the drainer buffer for the next operation. Without this cutoff a slow recorder-thread wake under contention would scoop up post-pattern bytes that on a fast wake would belong to the next event — producing partition drift in the cast.
- libx264 mp4 encoding is pinned to `-threads 1` and the concat demuxer's manifest is written to a per-call tempfile, so encodes are byte-stable across runs and concurrent encodes against the same frame set don't race.
- Glyph rasterization uses a bundled font (`include_bytes!`).
- Raw IO, diagnostic wall time, and playback time are separate layers in the trace.

## Authoring scenes

Scenes are small Rust binaries that drive a `Recorder`. Use
`Recorder::spawn` for an arbitrary local process; use `Recorder::start`
for a Docker-backed shell session. Prefer content-aware gates
(`send_raw_wait_for`, plus consumer-defined helpers like
`wait_for_prompt`, `ps2_enter`) over fixed sleeps and bare `Key::Enter`
— the recorder's default settle is microseconds and not a substitute
for syncing on a known byte pattern. Use presentation helpers only for
output that does not affect shell state (comments, blank prompt lines,
clear boundaries).

Working examples live in `examples/` and the `tint-scenes/scenes/`
consumer crate.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream
Vera license; see `assets/fonts/LICENSE-DejaVu.txt`.
