# term-recorder

Deterministic GIF/MP4 recorder for scripted terminal demos. Spawns any
interactive process under a PTY, captures raw IO, emits an asciinema
cast whose timestamps come from virtual presentation time, then renders
cast → PNG frames → GIF/MP4. Output is byte-stable across runs.

## Quickstart

Write a scene file targeting a local shell — no Docker required:

```
# demo.scene
Version 1
SetSpawn "bash" "--noprofile" "--norc" "-i"
SetEnv "PS1" "$ "
SetEnv "TERM" "xterm-256color"

WaitForPrompt
Run "echo hello"
Run "ls /tmp | head -3"
Sleep 800ms
```

Then render it to a GIF in one call:

```bash
$ term-recorder record demo.scene --out demo.gif
```

That's the whole loop. The local-PTY target (`SetSpawn`) is the
default and works anywhere `bash` is on `PATH`. Docker-backed targets
(`SetWarm` / `SetCold`) exist for hermetic CI / golden-gating; see
[`docs/scene-grammar.md`](docs/scene-grammar.md) for the full grammar.

## Live recording

Don't have a script? Just press the key:

```bash
$ term-recorder rec --out demo.cast
[recording → demo.cast]  type 'exit' or Ctrl-D to stop
$ echo hello
hello
$ exit
wrote demo.cast (3 events, 4.2s)

$ term-recorder render demo.cast demo.gif
```

`rec` spawns your `$SHELL` (falling back to `bash`) under a PTY,
puts the host stdin in raw mode, and streams every byte from the
shell into the cast — like `asciinema rec`. The session ends when
you `exit` or hit Ctrl-D.

**Determinism scope.** Live casts use _real wall-clock_ dwells —
the cast's timeline is a faithful record of what was typed when, not
a reproducible derivation. The downstream `cast → media` render is
still byte-stable, so receipts attest the render exactly as for
scripted recordings.

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
library is process-agnostic — it works with any interactive CLI you
can spawn under a PTY.

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
    { "kind": "contains_text", "text": "$ echo hello" },
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
term-recorder rec     [--out PATH]                            # live: record your real terminal session
term-recorder record  <scene> --out <cast|media>              # scripted: run a .scene file
term-recorder render  <cast>  <out>  [--receipt R] [--spec S] # cast → MP4/GIF (one call)
term-recorder stitch  --out OUT INPUT...                      # concatenate casts (the cast-monoid ⊕)
term-recorder verify  --receipt R --cast C [--spec S]         # check a receipt
term-recorder check   --cast C --spec S                       # check a spec
```

Per-stage pipeline tools sit under `term-recorder debug ...`:

```bash
term-recorder debug snapshot          <cast> <out_dir>          # cast → snapshot JSON
term-recorder debug paint             <snap_dir> <out_dir>      # snapshots → PNGs
term-recorder debug encode            <frames> <timing> <out>   # PNGs → MP4/GIF
term-recorder debug compare-snapshots <baseline> <candidate>    # frame-by-frame diff
term-recorder debug inspect           <snapshot>                # ASCII-render to terminal
```

`render` chains `snapshot → paint → encode` in memory; the `debug`
subcommands expose each stage separately when you want intermediate
artifacts on disk (typically: layered hash gates that pin every stage
independently). `verify` and `check` are the two attestation verifiers
(provenance and behavior).

## Pipeline

```
Recorder API           → cast    PTY driver + scripted input + OSC responder
src/snapshot_replay    → JSON    vt100 + OSC tracker → per-frame snapshots
src/paint.rs           → PNGs    JSON + bundled font → image
src/encode.rs          → GIF/MP4 ffmpeg concat-demuxer
```

## Setup

```bash
cargo build --release
cargo test
```

Requires `cargo` (and `ffmpeg` for encode-stage tests). Recording into a
sandboxed shell additionally needs `docker`.

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

The `goldens/` directory and `make verify-goldens` / `make
bless-goldens` targets ship in the consumer crate that drives this
library, not here — `term-recorder` itself ships only the recorder
primitives, the scene runner, and the render pipeline.

## Determinism

- Cold-container mode (`SetCold`) pins the recording shell to a chosen image (e.g. `debian:12-slim`) with a fresh `$HOME` and no host `$PATH` leakage; local mode (`SetSpawn`, the default) inherits the host environment so determinism guarantees apply only to the render side (Arrow B).
- PTY winsize is fixed before exec; `portable-pty` handles the platform-correct fork/exec/ctty dance.
- The driver answers OSC 10/11 color queries with canned RGB, so the recorded process runs unmodified.
- Cast timestamps come from cumulative `dwell_ms`, never wall clock.
- `wait_for` cuts off the captured event at the pattern's end byte; bytes that arrive after the pattern stay in the drainer buffer for the next operation. Without this cutoff a slow recorder-thread wake under contention would scoop up post-pattern bytes that on a fast wake would belong to the next event — producing partition drift in the cast.
- libx264 mp4 encoding is pinned to `-threads 1` and the concat demuxer's manifest is written to a per-call tempfile, so encodes are byte-stable across runs and concurrent encodes against the same frame set don't race.
- Glyph rasterization uses a bundled font (`include_bytes!`).
- Raw IO, diagnostic wall time, and playback time are separate layers in the trace.

## Authoring scenes

Most scenes are `.scene` files (see the [Quickstart](#quickstart) and
[`docs/scene-grammar.md`](docs/scene-grammar.md) for the v1 grammar).
The DSL targets a local process by default (`SetSpawn`); switch to
`SetWarm` / `SetCold` when you need hermetic recording. Power users
can drop down to the `Recorder` library directly from a Rust binary —
`Recorder::spawn` for an arbitrary local process, `Recorder::start`
for the Docker-backed path.

Either way, prefer content-aware gates (`WaitFor` / `WaitForPrompt`
in the DSL, `send_raw_wait_for` in the library) over fixed sleeps and
bare `Key::Enter` — the recorder's default settle is microseconds and
not a substitute for syncing on a known byte pattern. Use presentation
helpers (`Present` / `PresentTyped`) only for output that does not
affect shell state: comments, blank prompt lines, clear boundaries.

Working library examples live in `examples/`.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream
Vera license; see `assets/fonts/LICENSE-DejaVu.txt`.
