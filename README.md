# ptyroom

`ptyroom` is a shared terminal room that leaves behind a durable trace.
Start one PTY, let other terminals join it, and keep the result as a
`.ptytrace` artifact that can be rendered, verified, or bundled later.

It is deliberately smaller than tmux: there is one shared PTY, one shared
view, and intentionally merged input. That makes it useful for demos,
pairing, debugging, teaching, and chaotic "everyone can type" sessions.

The top-level command is `ptyroom`:

```bash
ptyroom host --listen 127.0.0.1:7373 --out /tmp/room.ptytrace bash
ptyroom join 127.0.0.1:7373
```

The room is the live experience. The trace is the durable evidence.
Render, verify, and bundle commands are downstream tools for that trace.

The repository is named `ptyroom` because the shared room is the primary
user-facing workflow. The workspace is split along the command algebra:
`ptytrace`, `ptyrender`, `ptyrecord`, and `ptyroom` are separate crates.
The lower-level trace crate, file format, and raw recorder command are
still named `ptytrace`.

## Quickstart

Install from GitHub:

```bash
cargo install --git https://github.com/corygabrielsen/ptyroom --package ptyroom --locked
```

Build from a checkout:

```bash
cargo build --release --workspace --bins
```

Run the local smoke demo:

```bash
scripts/smoke-local.sh
```

Or install just the room command locally:

```bash
cargo install --path crates/ptyroom --locked
```

Install artifact tools when you need them:

```bash
# from a checkout
cargo install --path crates/ptytrace --locked
cargo install --path crates/ptyrender --locked
cargo install --path crates/ptyrecord --locked

# from GitHub
cargo install --git https://github.com/corygabrielsen/ptyroom --package ptytrace --locked
cargo install --git https://github.com/corygabrielsen/ptyroom --package ptyrender --locked
cargo install --git https://github.com/corygabrielsen/ptyroom --package ptyrecord --locked
```

Host a local room:

```bash
ptyroom host \
    --listen 127.0.0.1:7373 \
    --out /tmp/ptyroom-demo.ptytrace \
    --cols 100 \
    --rows 30 \
    bash
```

Join from another terminal:

```bash
ptyroom join 127.0.0.1:7373
```

Watch read-only from another terminal:

```bash
ptyroom watch 127.0.0.1:7373
```

Both the host and joined clients type into the same child PTY. The host
terminal participates by default; use `--no-local-input` only when joined
clients should be the exclusive input source. Watch clients see the same
output stream but never send input and never participate in the shared
PTY size negotiation.

For a more chaotic local demo, run the host in one terminal and start two
or more joins from other terminals. Everyone sees the same PTY and all
input goes to the same child process.

Fully interactive joins reserve `Ctrl-]` as a local prefix. Press
`Ctrl-] .` to detach from the room, `Ctrl-] ?` for help, `Ctrl-] r` to
redraw the local viewport, or `Ctrl-] Ctrl-]` to send a literal `Ctrl-]`
to the shared PTY.

Interactive joins use a tmux-like size rule. The shared PTY uses the
smallest active rendering terminal size, and joined terminals reserve one
local status row that is not part of the remote PTY. This keeps zoomed-in
participants from seeing a broken oversized layout.

When the room ends, the output path is a normal trace:

```bash
ptyrender /tmp/ptyroom-demo.ptytrace room.gif
```

Rendering GIF/MP4 output requires `ffmpeg` on `PATH`.

For remote use, bind loopback and carry the TCP stream through SSH,
WireGuard, or another authenticated tunnel. The built-in transport has no
authentication or encryption. Shared-terminal details are in
[`docs/shared-terminals.md`](docs/shared-terminals.md).

## Status

This is early software intended for local demos and trusted networks. The
default bind is loopback, non-loopback binds require an explicit unsafe
flag, and the wire protocol is not an authentication or encryption layer.

## What It Is Not

`ptyroom` is not a tmux replacement, persistent terminal multiplexer,
authorization server, or secure remote shell. It is one shared PTY with a
durable trace. Use SSH, WireGuard, or an equivalent trusted channel for
remote access.

## Which Command Should I Use?

Start with `ptyroom` when you want the shared-terminal experience:

```bash
ptyroom host [--listen 127.0.0.1:0] [cmd]
ptyroom join 127.0.0.1:7000
ptyroom watch 127.0.0.1:7000
ptyroom ctl 127.0.0.1:7000 queue add "next prompt"
ptyroom ctl 127.0.0.1:7000 queue next
```

Use `watch` when an observer should see the room without sending input or
shrinking the shared PTY (demos, recordings, teaching audiences). Use
`ctl queue` to enqueue text that should later be typed into the shared
PTY — the original motivation is wiring Claude Code's `Stop` hook to
`queue next` so queued prompts are auto-submitted at every turn
boundary (see [`docs/shared-terminals.md`](docs/shared-terminals.md)).

Use the other binaries when you are working with the durable artifact:

| Need                             | Command                                        |
| -------------------------------- | ---------------------------------------------- |
| Run one command and keep a trace | `ptytrace <command...>`                        |
| Record an exploratory shell      | `ptytrace capture --out demo.ptytrace`         |
| Run a scripted recording         | `ptytrace run demo.script --out demo.ptytrace` |
| Render a trace to media          | `ptyrender demo.ptytrace demo.gif`             |
| Capture, render, and package     | `ptyrecord --out demo.ptyrecord <command...>`  |
| Verify a witness or contract     | `ptyrender verify ...` / `ptytrace check ...`  |

The `ptyrender` crate owns the replay, frame, paint, encode, and witness
pipeline. The `ptytrace` binary stays focused on producing and checking
trace artifacts.

## Workspace Layout

The four Cargo packages mirror the data flow:

```text
ptyroom   -> .ptytrace
ptytrace  -> .ptytrace
ptyrender -> media + witness
ptyrecord -> .ptyrecord bundle
```

- `crates/ptyroom`: shared terminal room CLI.
- `crates/ptytrace`: trace schema, PTY capture, scripts, contracts, and raw
  recorder CLI.
- `crates/ptyrender`: replay, frame, paint, encode, witness, and renderer CLI
  for trace-derived media.
- `crates/ptyrecord`: composed capture/render/bundle CLI.

## Common Workflows

### Script a Reproducible Recording

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
ptytrace run demo.script --out demo.ptytrace
ptyrender demo.ptytrace demo.gif
```

`SetSpawn` is the normal local starting point. Docker-backed `SetWarm`
and `SetCold` targets are available when a recording needs a hermetic
environment. The full DSL is documented in
[`docs/script-grammar.md`](docs/script-grammar.md).

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

`ptyrender` runs replay, paint, and encode in one step. The library
modules expose those stages separately when intermediate artifacts are
useful.

### Inspect the Room Protocol

`ptyroom host` owns the child PTY and accepts joined clients.
`ptyroom join` connects a local terminal to that room. The byte protocol
between those two subcommands is documented in
[`docs/ptyroom-protocol.md`](docs/ptyroom-protocol.md).

## Verification

The artifact pipeline has three verification layers:

- Witnesses prove that media bytes reproduce from a trace, render config,
  font, toolchain, and ffmpeg identity.
- Contracts check behavior by replaying the trace and evaluating predicates
  over the terminal state.
- Attestations bind an external provider-specific claim to the trace hash.

Render with a witness:

```bash
ptyrender demo.ptytrace demo.gif --receipt demo.gif.witness.json
ptyrender verify --witness demo.gif.witness.json --trace demo.ptytrace
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
    --script demo.script \
    --attestation demo.attestation.json
```

The provenance model is described in
[`docs/provenance-anchors.md`](docs/provenance-anchors.md). The architecture
overview is in [`docs/crate-architecture.md`](docs/crate-architecture.md).

## Library API

Render a trace from Rust:

```rust
ptyrender::render("demo.ptytrace")?
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

Working examples live in `crates/ptytrace/examples/` and
`crates/ptyrender/examples/`. The recorder library is process-agnostic:
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
cargo build --workspace --bins
cargo test --workspace
```

Requires Rust/Cargo. Rendering and encode-stage tests require `ffmpeg`.
Docker-backed script targets require `docker`.

Useful local checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace --bins
PTYROOM_SMOKE_SKIP_BUILD=1 scripts/smoke-local.sh
cargo doc --workspace --no-deps
cargo sort --workspace --check
cargo machete
git diff --check
```

Stress coverage for PTY timing primitives lives in
[`crates/ptytrace/tests/ptytrace_stress.rs`](crates/ptytrace/tests/ptytrace_stress.rs)
and uses the generic test child in
[`crates/ptytrace/tests/fixtures/stress_child.rs`](crates/ptytrace/tests/fixtures/stress_child.rs).
Consumer-specific golden media belongs in consumer crates, not in the
`ptyroom` repository.

Useful focused checks while working on `ptyroom`:

```bash
cargo test -p ptytrace pty::room_protocol --lib
cargo test -p ptytrace pty::connect --lib
cargo test -p ptytrace pty::share --lib
cargo test -p ptyroom --test ptyroom_transport_cli
```

## Documents

- [`docs/shared-terminals.md`](docs/shared-terminals.md): operator model
  for `ptyroom` rooms, local controls, geometry, cleanup, and security.
- [`docs/ptyroom-protocol.md`](docs/ptyroom-protocol.md): byte-level host
  and join protocol.
- [`docs/script-grammar.md`](docs/script-grammar.md): reproducible
  recording script DSL.
- [`docs/ptyrecord-format.md`](docs/ptyrecord-format.md): portable bundle
  format.
- [`docs/provenance-anchors.md`](docs/provenance-anchors.md): witness,
  contract, and attestation model.
- [`docs/determinism-audit.md`](docs/determinism-audit.md): render
  determinism assumptions and risks.
- [`docs/crate-architecture.md`](docs/crate-architecture.md): workspace
  layering and invariants.
- [`docs/publishing.md`](docs/publishing.md): crates.io publish order and
  release checks.
- [`SECURITY.md`](SECURITY.md): supported security boundary and reporting.

## License

MIT - see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera
license; see `crates/ptyrender/assets/fonts/LICENSE-DejaVu.txt`.
