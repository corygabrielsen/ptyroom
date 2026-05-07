# ptytrace

`ptytrace` records interactive terminal sessions under a PTY, stores the
raw trace, and renders that trace into GIF/MP4. It is built for repeatable
terminal media, verifiable artifacts, and shared terminal sessions.

The central artifact is a `.ptytrace`: an asciinema-shaped event log that
can be inspected, verified, stitched, rendered again, or bundled with media
as a `.ptyrecord`.

## Command Map

Main commands:

```bash
ptytrace <command...>                                      # command -> trace
ptytrace capture [--out PATH]                             # live shell -> trace
ptytrace run <script> --out <trace|media>                 # script -> trace/media
ptyrender <trace> <out.gif|out.mp4> [--receipt R]          # trace -> media
ptyrecord [--out OUT.ptyrecord] <command...>               # command -> bundle
ptyroom host [--listen 127.0.0.1:0] [cmd]                  # shared room host
ptyroom join 127.0.0.1:7000                                # shared room client
```

Lower-level or specialized commands:

```bash
ptyshare [--listen 127.0.0.1:0] [--out S.ptytrace] [cmd]   # shared PTY host
ptyconnect 127.0.0.1:7000                                  # shared PTY client
ptytrace render <trace> <out> [--receipt R] [--spec S]
ptytrace verify --witness R --trace T [--contract C] [--attestation A]
ptytrace check --trace T --contract C
ptytrace stitch --out OUT INPUT...
ptytrace attest file --trace T --out A
```

Debug pipeline commands:

```bash
ptytrace debug replay <trace> <out_dir>                    # trace -> frame JSON
ptytrace debug paint <snap_dir> <out_dir>                  # frames -> PNGs
ptytrace debug encode <frames> <timing> <out>              # PNGs -> GIF/MP4
ptytrace debug compare-snapshots <baseline> <candidate>
ptytrace debug inspect <frame>
```

## Quickstart

Write a script that targets a local shell:

```text
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

Render it to a GIF:

```bash
ptytrace run demo.script --out demo.gif
```

`SetSpawn` is the normal local starting point. Docker-backed `SetWarm`
and `SetCold` targets are available when a recording needs a hermetic
environment. The full DSL is documented in
[`docs/script-grammar.md`](docs/script-grammar.md).

## Common Workflows

### Record a Command

Use raw `ptytrace` when you want the durable trace and nothing else:

```bash
ptytrace htop
# wrote recording-1778082000.ptytrace
```

Use `ptyrecord` when you want a portable bundle containing the trace,
MP4 media, witness data, and selectable text:

```bash
ptyrecord --out deploy.ptyrecord ssh deploy@example.com
```

To package an already-rendered trace and MP4:

```bash
ptyrecord --trace-in demo.ptytrace \
    --media-in demo.mp4 \
    --witness-in demo.mp4.receipt.json \
    --out demo.ptyrecord
```

The trace remains the durable artifact. Media and bundles are repeatable
products derived from it. The `.ptyrecord` format is documented in
[`docs/ptyrecord-format.md`](docs/ptyrecord-format.md).

### Record a Live Shell

Use `capture` when you do not have a script yet:

```bash
ptytrace capture --out demo.ptytrace
# type normally, then `exit` or Ctrl-D
ptyrender demo.ptytrace demo.gif
```

Live command traces use wall-clock dwell times. The downstream
`trace -> media` render is still byte-stable, but the captured timeline
is a faithful record of what happened rather than a reproducible scripted
derivation.

### Render a Trace

```bash
ptyrender demo.ptytrace demo.gif
ptyrender demo.ptytrace demo.mp4 --receipt demo.mp4.witness.json
```

`ptyrender` and `ptytrace render` run replay, paint, and encode in one
step. The `ptytrace debug ...` commands expose those stages separately
when intermediate artifacts are useful.

### Share a Terminal Room

Use `ptyroom` for collaborative terminal sessions:

```bash
ptyroom host --listen 127.0.0.1:7373 --out /tmp/room.ptytrace bash
ptyroom join 127.0.0.1:7373
```

The host terminal is interactive by default. Use `--no-local-input` only
when joined clients should be the exclusive input source.

`ptyshare` and `ptyconnect` are the lower-level transport tools behind
`ptyroom`. Shared-terminal behavior is documented in
[`docs/shared-terminals.md`](docs/shared-terminals.md), and the byte
protocol is documented in
[`docs/ptyshare-protocol.md`](docs/ptyshare-protocol.md).

## Verification

`ptytrace` has three verification layers:

- Witnesses prove that media bytes reproduce from a trace, render config,
  font, toolchain, and ffmpeg identity.
- Contracts check behavior by replaying the trace and evaluating predicates
  over the terminal state.
- Attestations bind an external provider-specific claim to the trace hash.

Render with a witness:

```bash
ptyrender demo.ptytrace demo.gif --receipt demo.gif.witness.json
ptytrace verify --witness demo.gif.witness.json --trace demo.ptytrace
```

Check a behavioral contract:

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
```

Attach a local unsigned attestation:

```bash
ptytrace attest file --trace demo.ptytrace --out demo.attestation.json
ptyrender demo.ptytrace demo.gif \
    --receipt demo.gif.witness.json \
    --spec demo.contract.json \
    --attestation demo.attestation.json
```

The provenance model is described in
[`docs/provenance-anchors.md`](docs/provenance-anchors.md). The architecture
overview is in [`docs/crate-architecture.md`](docs/crate-architecture.md).

## Library API

Render a trace from Rust:

```rust
ptytrace::render("demo.ptytrace")?
    .font_size(40.0)
    .width(824)
    .to_path("demo.gif")?;
```

Drive an interactive process directly:

```rust
use std::time::Duration;
use ptytrace::pty::{PtyTracer, PtyTracerConfig};

let mut rec = PtyTracer::spawn(PtyTracerConfig::default(), &["bash"])?;
rec.send_raw_wait_for(
    &[],
    Duration::ZERO,
    b"$ ",
    Duration::from_secs(2),
    "prompt",
)?;
rec.type_text("echo hello", Duration::from_millis(35))?;
rec.send_raw_wait_for(
    b"\n",
    Duration::from_millis(300),
    b"$ ",
    Duration::from_secs(2),
    "echo prompt",
)?;
rec.stop()?.write("hello.ptytrace")?;
```

Working examples live in `examples/`. The library is process-agnostic:
it can drive any interactive CLI that can run under a PTY.

## Determinism Model

Scripted traces use virtual presentation time. Live command traces use
real wall-clock time. Rendering a trace to frames and media is intended
to be byte-stable under the pinned render identity.

Important invariants:

- PTY size is fixed before exec.
- Trace timestamps come from cumulative intended dwell, not wall-clock.
- `wait_for` cuts captured output at the matched pattern boundary.
- The recorder answers OSC 10/11 color queries with deterministic values.
- Rasterization uses a bundled font.
- libx264 encoding is pinned to single-threaded concat-demuxer inputs.
- Raw IO, diagnostic wall time, and playback time are separate layers.

The deeper determinism audit is in
[`docs/determinism-audit.md`](docs/determinism-audit.md).

## Development

```bash
cargo build --release
cargo test
```

Requires `cargo`. Encode-stage tests require `ffmpeg`. Docker-backed script
targets require `docker`.

Useful local checks:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --bins
```

Stress coverage for PTY timing primitives lives in
[`tests/ptytrace_stress.rs`](tests/ptytrace_stress.rs) and uses the generic
test child in [`src/bin/stress_child.rs`](src/bin/stress_child.rs).
Consumer-specific golden media belongs in consumer crates, not in
`ptytrace`.

## Documents

- [`docs/script-grammar.md`](docs/script-grammar.md)
- [`docs/shared-terminals.md`](docs/shared-terminals.md)
- [`docs/ptyshare-protocol.md`](docs/ptyshare-protocol.md)
- [`docs/ptyrecord-format.md`](docs/ptyrecord-format.md)
- [`docs/provenance-anchors.md`](docs/provenance-anchors.md)
- [`docs/determinism-audit.md`](docs/determinism-audit.md)
- [`docs/crate-architecture.md`](docs/crate-architecture.md)

## License

MIT - see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera
license; see `assets/fonts/LICENSE-DejaVu.txt`.
