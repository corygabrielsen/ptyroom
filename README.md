# ptytrace

Deterministic PTY recorder and renderer for interactive terminal
sessions. It can record any command under a PTY, preserve the raw trace,
and render that trace into GIF/MP4 with a reproducibility witness.
Scripted traces use virtual presentation time; live command traces use
wall-clock timing.

## Quickstart

Write a script file targeting a local shell — no Docker required:

```
# demo.script
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
$ ptytrace run demo.script --out demo.gif
```

That's the whole loop. The local-PTY target (`SetSpawn`) is the
normal starting point and works anywhere `bash` is on `PATH`.
Docker-backed targets
(`SetWarm` / `SetCold`) exist for hermetic CI / golden-gating; see
[`docs/script-grammar.md`](docs/script-grammar.md) for the full grammar.

## Command Recording

Raw `ptytrace` records a command and writes only the durable trace:

```bash
$ ptytrace htop
[recording → recording-1778082000.ptytrace]  type 'exit' or Ctrl-D to stop
...
wrote recording-1778082000.ptytrace
```

`ptyrecord` is the composed workflow. It captures the command, paints
frames while the session is still running, encodes those stitched
frames to browser-controllable MP4, and writes one `.ptyrecord` bundle
containing the `.ptytrace`, media, witness, and selectable text:

```bash
$ ptyrecord --out deploy.ptyrecord ssh deploy@example.com
[recording → deploy.ptytrace]
...
wrote deploy.ptyrecord + embedded trace deploy.ptytrace + media deploy.mp4
```

To package an already-rendered trace and MP4, use bundle mode:

```console
$ ptyrecord --trace-in demo.ptytrace --media-in demo.mp4 \
    --witness-in demo.mp4.receipt.json \
    --out demo.ptyrecord
```

Algebraically, `ptyrecord(command) = bundle(ptytrace(command),
ptyrender(ptytrace(command)))`. The trace remains the artifact you can
keep, inspect, verify, stitch, or render again later.

## Live Shell

Don't have a script? Just press the key:

```bash
$ ptytrace capture --out demo.ptytrace
[recording → demo.ptytrace]  type 'exit' or Ctrl-D to stop
$ echo hello
hello
$ exit
wrote demo.ptytrace (3 events, 4.2s)

$ ptyrender demo.ptytrace demo.gif
```

`capture` spawns your `$SHELL` (falling back to `bash`) under a PTY,
puts the host stdin in raw mode, and streams every byte from the
shell into the trace — like `asciinema rec`. The session ends when
you `exit` or hit Ctrl-D.

**Determinism scope.** Live command traces use _real wall-clock_ dwells —
the trace's timeline is a faithful record of what was typed when, not
a reproducible derivation. The downstream `trace → media` render is
still byte-stable, so receipts attest the render exactly as for
scripted recordings.

## Library use

Render an existing trace to media in one call:

```rust
ptytrace::render("demo.ptytrace")?
    .font_size(40.0)
    .width(824)
    .to_path("demo.gif")?;
```

Output format is inferred from the path extension (`.mp4` or `.gif`).
Intermediate frame JSON and PNG frames live in a tempdir for the
duration of the call.

Drive an interactive process and produce a trace:

```rust
use std::time::Duration;
use ptytrace::pty::{PtyTracer, PtyTracerConfig};

let mut rec = PtyTracer::spawn(PtyTracerConfig::default(), &["bash"])?;
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
rec.stop()?.write("hello.ptytrace")?;
```

A working version is at `examples/generic_shell.rs`. The ptytrace
library is process-agnostic — it works with any interactive CLI you
can spawn under a PTY.

## Verifiable artifacts

Determinism isn't just an internal property — it's externally checkable
through three composable sidecars:

**Witnesses (render reproduction).** A witness is a JSON sidecar that
records the trace hash, render config, tool / ffmpeg / font versions,
and output hash. Re-rendering on any machine with the same identity
should produce the same output bytes:

```rust
let witness = ptytrace::render("demo.ptytrace")?
    .font_size(40.0)
    .to_path_with_receipt("demo.gif")?;
witness.write("demo.gif.witness.json")?;
```

```bash
ptytrace verify --witness demo.gif.witness.json --trace demo.ptytrace
# MATCH  →  exit 0
# TRACE_DIFFERS / ENV_DIFFERS / OUTPUT_DIFFERS  →  exit 1
```

**Specs (behavior).** A contract is a JSON file listing predicates the
trace must satisfy. The verifier replays the trace and re-evaluates:

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
ptytrace check --trace demo.ptytrace --contract demo.contract.json
# PASS / FAIL per predicate;  exit 0 only if every predicate passes
```

**Attestations (external provenance).** An attestation is a detached
claim that some provider bound itself to `hash(trace)`. The built-in
`file` provider is unsigned and local: it is useful for demos and
plumbing tests, but it does not prove a remote identity. Stronger
providers such as SSH, KMS, TPM, CI/OIDC, or transparency logs can use
the same schema and target-digest check.

```bash
ptytrace attest file --trace demo.ptytrace --out demo.attestation.json
```

**Composition.** A witness can embed both `contract_sha256` and
`attestation_sha256` so one `verify` covers render reproduction,
behavior, and trace-targeted provenance:

```bash
ptyrender demo.ptytrace demo.gif \
    --receipt demo.gif.witness.json \
    --spec demo.contract.json \
    --attestation demo.attestation.json

ptytrace verify --witness demo.gif.witness.json \
    --trace demo.ptytrace \
    --contract demo.contract.json \
    --attestation demo.attestation.json
# MATCH only if trace hash matches, environment matches, re-render
# output matches, contract hash + predicates pass, attestation hash
# matches, and attestation.target_sha256 == trace_sha256.
```

The witness format is nix-derivation-shaped (provenance + bit-exact
reproduction); the contract is in-toto-policy-shaped (behavioral
assertions); the attestation is a provider-shaped provenance anchor.
Together they make the GIF/MP4, behavior, and external claim meet at
one trace digest. The provider substitution model is documented in
[`docs/provenance-anchors.md`](docs/provenance-anchors.md).

## CLI

Raw and composed command forms:

```bash
ptytrace <command...>                                      # command → trace
ptyrender <trace> <out.gif|out.mp4> [--receipt R]          # trace → media
ptyrecord [--out OUT.ptyrecord] <command...>               # command → trace + MP4 bundle
ptyshare [--listen 127.0.0.1:0] [--out S.ptytrace] [cmd]   # host shared PTY
ptyconnect 127.0.0.1:7000                                 # attach to ptyshare
```

`ptyshare` / `ptyconnect` are the first collaborative PTY primitive.
The host owns one PTY; host stdin and client input bytes are interleaved
into that PTY; PTY output is broadcast to all clients and written to a
`.ptytrace`. Slow clients get a bounded output backlog and are
disconnected rather than being allowed to stall the shared session.
The transport is intentionally small and does not provide authentication
or encryption; non-loopback binds require
`--allow-unauthenticated-public-bind`, and remote sharing should go
through SSH, WireGuard, or another authenticated tunnel.

`ptyconnect` works interactively and in pipelines: after piped stdin
closes, it keeps reading the shared session until the server closes.
The raw stream contract is documented in
[`docs/ptyshare-protocol.md`](docs/ptyshare-protocol.md).

The `ptytrace` binary also exposes named subcommands:

```bash
ptytrace capture [--out PATH]                            # live shell → trace
ptytrace run <script> --out <trace|media>                # scripted .script → trace or media
ptytrace render  <trace>  <out>  [--receipt R] [--spec S] [--attestation A|--attestation-out A]
ptytrace attest file --trace <trace> --out <attestation>       # trace → provenance sidecar
ptytrace stitch  --out OUT INPUT...                      # concatenate traces (the trace-monoid ⊕)
ptytrace verify  --witness R --trace C [--contract S] [--attestation A]
ptytrace check   --trace C --contract S                       # check a contract
```

Per-stage pipeline tools sit under `ptytrace debug ...`:

```bash
ptytrace debug replay         <trace> <out_dir>          # trace → frame JSON
ptytrace debug paint             <snap_dir> <out_dir>      # snapshots → PNGs
ptytrace debug encode            <frames> <timing> <out>   # PNGs → MP4/GIF
ptytrace debug compare-snapshots <baseline> <candidate>    # frame-by-frame diff
ptytrace debug inspect           <frame>                # ASCII-render to terminal
```

`ptyrender` and `ptytrace render` chain `frame → paint → encode` in
memory; the `debug` subcommands expose each stage separately when you
want intermediate artifacts on disk (typically: layered hash gates
that pin every stage independently). `verify` and `check` are the two
attestation verifiers (provenance and behavior).

## Pipeline

```
PTY driver API      → trace    process spawn + scripted input + OSC responder
src/frame_replay    → JSON    vt100 + OSC tracker → per-frame snapshots
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

The pipeline pins nine layer hashes per script under `goldens/<script>.json`
(concat-of-output-bytes, trace event count, final + concatenated frame
JSON, concatenated PNGs, mp4, gif, plus frame/png counts). Make
targets driven by the `pipeline-test` binary:

```bash
make verify-goldens     # one pass per script; PASS/FAIL per layer
make bless-goldens      # re-bake goldens; refuses if N=10 runs disagree
make characterize       # report determinism per layer per script
```

The bless agreement gate is the safety net against goldening
non-determinism: if any layer's hash differs across the N=10 verify
runs the bless aborts. Override `BLESS_RUNS=...` to tune the floor;
pass `PIPELINE_TEST_FLAGS='--scenes=foo,bar'` for subset operation.

PTY recorder timing primitives are exercised in
`tests/ptytrace_stress.rs` against a synthetic generic child
(`src/bin/stress_child.rs`). Tests assert the wait_for cutoff
contract directly and verify byte-stability under parallel load
and CPU contention. **Architectural rule:** these tests import
`ptytrace::*` only — never any consumer crate. The ptytrace
library is meant to be domain-generic; the seam is enforced by
where tests draw their dependencies.

The `goldens/` directory and `make verify-goldens` / `make
bless-goldens` targets ship in the consumer crate that drives this
library, not here — `ptytrace` itself ships only the recorder
primitives, the script runner, and the render pipeline.

The `.ptyrecord` bundle format is documented in
[`docs/ptyrecord-format.md`](docs/ptyrecord-format.md).

## Determinism

- Cold-container mode (`SetCold`) pins the recording shell to a chosen
  image (e.g. `debian:12-slim`) with a fresh `$HOME` and no host
  `$PATH` leakage; local mode (`SetSpawn`) inherits the host
  environment so determinism guarantees apply only to the render side
  (Arrow B).
- PTY winsize is fixed before exec; `portable-pty` handles the platform-correct fork/exec/ctty dance.
- The driver answers OSC 10/11 color queries with canned RGB, so the recorded process runs unmodified.
- Trace timestamps come from cumulative `dwell_ms`, never wall clock.
- `wait_for` cuts off the captured event at the pattern's end byte; bytes that arrive after the pattern stay in the drainer buffer for the next operation. Without this cutoff a slow recorder-thread wake under contention would scoop up post-pattern bytes that on a fast wake would belong to the next event — producing partition drift in the trace.
- libx264 mp4 encoding is pinned to `-threads 1` and the concat demuxer's manifest is written to a per-call tempfile, so encodes are byte-stable across runs and concurrent encodes against the same frame set don't race.
- Glyph rasterization uses a bundled font (`include_bytes!`).
- Raw IO, diagnostic wall time, and playback time are separate layers in the trace.

## Authoring Scripts

Most recordings start as `.script` files (see the [Quickstart](#quickstart) and
[`docs/script-grammar.md`](docs/script-grammar.md) for the v1 grammar).
Use `SetSpawn` for local processes; switch to `SetWarm` / `SetCold`
when you need hermetic recording. Power users
can drop down to the `PtyTracer` library directly from a Rust binary —
`PtyTracer::spawn` for an arbitrary local process, `PtyTracer::start`
for the Docker-backed path.

Either way, prefer content-aware gates (`WaitFor` / `WaitForPrompt`
in the DSL, `send_raw_wait_for` in the library) over fixed sleeps and
bare `Key::Enter` — the default settle is microseconds and
not a substitute for syncing on a known byte pattern. Use presentation
helpers (`Present` / `PresentTyped`) only for output that does not
affect shell state: comments, blank prompt lines, clear boundaries.

Working library examples live in `examples/`.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream
Vera license; see `assets/fonts/LICENSE-DejaVu.txt`.
