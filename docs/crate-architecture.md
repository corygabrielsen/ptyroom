# Crate Architecture

`ptytrace` is a reusable deterministic PTY session recorder. Its
primitives are designed to compose into scripted recordings of any
interactive CLI; consumer-specific scenes live above this library, not
inside it.

## Design Goal

Record scripted terminal sessions as reproducible artifacts:

- capture real PTY input and output as raw evidence;
- keep playback time independent from wall-clock capture time;
- allow presentation-only bytes, but mark them as synthetic;
- verify the rendered terminal state before treating a recording as good;
- render the same trace into MP4, GIF, snapshots, or test fixtures.

The core should be useful for any CLI tool. Product-specific scenes should
live above it.

## Layers

```text
app scenes
  Domain story: commands, labels, timing taste, expected states.

script helpers
  Small reusable beats: type a line, press Enter, wait for a prompt,
  hold a frame, add presentation-only text.

ptytrace core
  PTY spawn, typed input, raw byte capture, content-aware waits,
  virtual presentation time, asciinema-compatible trace output.

raw evidence and proof layers
  Append-only IO log, verified semantic transitions, monotonic timeline,
  detached provenance anchors over trace digests.

terminal model and verification
  Replay traces into snapshots, then check predicates over text, colors,
  cursor-visible state, and forbidden regressions.

rendering
  Convert snapshots into PNG frames, then encode GIF/MP4.
```

## Core Contract

The ptytrace core owns terminal mechanics:

- process spawning under a PTY;
- terminal rows and columns;
- byte writes;
- byte capture;
- content-aware waits;
- virtual dwell;
- trace construction.

The ptytrace core must not own product semantics:

- no consumer-specific identifiers (theme names, command names, etc.);
- no consumer-specific UI conventions;
- no consumer-specific file names;
- no app-specific shortcuts hidden below the script layer.

## Time Model

There are two clocks.

Wall-clock time is diagnostic and synchronization-only. It answers:

- has the child emitted the prompt yet?
- has the alternate screen appeared?
- did a command hang?

Presentation time is the trace timeline. It answers:

- how long should this frame remain visible?
- how quickly should typed text appear?
- how long should the viewer digest a state change?

Changing CPU speed should change wall-clock capture duration, not playback
timing.

## Output Sources

Visible bytes have source identity:

```text
input                bytes written to the child PTY
output               bytes captured from the child PTY
presentation_output  bytes inserted by the script for visual structure
```

Both `output` and `presentation_output` render into the final trace.
Only `output` came from the child process.

This distinction is non-negotiable. It lets scenes use labels and visual
spacing without pretending those bytes were produced by the CLI being
demonstrated.

## Public API Shape

The generic API should stay small:

```rust
let mut r = PtyTracer::spawn(config, &["bash", "-i"])?;
r.type_text("echo hello", char_dwell)?;
r.send_raw_wait_for(b"\n", enter_dwell, b"$ ", timeout, "prompt")?;
r.push_presentation_output("# heading", heading_dwell)?;
let trace = r.stop()?;
```

The Docker convenience path is an adapter:

```rust
let mut r = PtyTracer::start(PtyTracerConfig {
    shell: ShellProfile::simple(),
    ..PtyTracerConfig::default()
})?;
```

Consumer-specific script helpers should remain outside the ptytrace core.

## Command Algebra

The user-facing tools should separate primitives from composed workflows:

```text
ptytrace : Command -> Trace
ptyrender: Trace x RenderOptions -> Media x Witness
ptyrecord: Command x RenderOptions -> PtyRecord
```

`ptytrace htop` and `ptytrace ssh ...` are the raw operation: run a
command under a PTY and preserve the resulting trace. They should not
need to render media.

`ptyrender trace.ptytrace out.gif` is the pure replay/render
operation. It can run at any later time, on another machine, or inside
a verifier.

`ptyrecord htop` and `ptyrecord ssh ...` are convenience composition:

```text
ptyrecord(command, options) =
  bundle(ptytrace(command), ptyrender(ptytrace(command), options))
```

`ptyrecord --trace-in T --media-in M --witness-in W --out R` exposes the
same final `bundle(...)` step for static-site and release pipelines that
already rendered media from a trace.

That composition can later be optimized into a streaming pipeline so
the GIF/MP4 is nearly ready when capture ends, but the optimization
must preserve the same algebra: the trace remains the durable artifact,
rendering remains repeatable from that artifact, and the `.ptyrecord`
bundle is just a portable packaging layer.

The first live optimization is frame stitching: `ptyrecord` feeds live
capture output into the shared `ReplayState` and paints frames during
the session, then only encodes and bundles at the end. This removes the
post-session replay/paint pass without changing the trace or witness
algebra.

## Current CLI Shape

The current crate installs five user-facing binaries:

- `ptytrace`: the raw primitive plus named low-level subcommands.
- `ptyrender`: the renderer that turns a trace into GIF/MP4 media and
  optional witnesses.
- `ptyrecord`: the composed command recorder that captures, renders MP4,
  and writes a `.ptyrecord` bundle.
- `ptyshare`: host one shared PTY over TCP, interleave host and client
  input, broadcast output to clients, and write the output trace.
- `ptyconnect`: attach a local terminal to a `ptyshare` TCP session.

`ptytrace render` remains available as the low-level subcommand form of
`ptyrender`.

`ptyshare` is transport plumbing, not a trust primitive. It defaults to
loopback, refuses non-loopback binds without an explicit unsafe flag, and
should be paired with SSH, WireGuard, or another authenticated tunnel
before crossing a machine boundary. Client output is nonblocking with a
bounded backlog: a slow observer can be disconnected, but it cannot stop
the PTY owner, recorder, or other clients from making progress.
The byte-level contract is in [`ptyshare-protocol.md`](ptyshare-protocol.md).

## Future Package Split

Prefer package names that make the layering explicit even if installed
binaries are short:

- `ptytrace`: trace schema, PTY capture, script runner, provenance
  anchors over trace digests, and the raw `ptytrace` binary.
- `ptyrender`: frame replay, paint, encode, render witnesses,
  contracts over rendered terminal state, and the `ptyrender` CLI.
- `ptyrecord`: thin CLI package that depends on both lower
  layers and exposes the composed `ptyrecord` CLI.

`ptyrecord` should not own trace or render logic. It is a UX shell
around the two lower-level operations.

## Verification Model

A recording is not "good" because encoding succeeded. It is good when:

- raw IO is closed;
- expected transitions are replay-verified;
- presentation timestamps are monotonic;
- rendered snapshots satisfy the script contract;
- any external provenance sidecar targets the same trace digest as the witness.

The verification layer should be reusable. A consumer should be able to assert
that text appeared, that text never appeared, that a color was reached, or that
the final frame matches a desired loop state.

Provenance anchors are provider-shaped and detached from the recorder.
The witness commits to an attestation file hash, and verification checks
the load-bearing law: `attestation.target_sha256 == witness.trace_sha256`.
Provider-specific trust, such as SSH host identity or KMS signatures, lives
behind `AttestationProvider` / `AttestationVerifier` implementations.
The formal substitution model is in
[`provenance-anchors.md`](provenance-anchors.md).

## Publication Bar

Before publishing, this project should have:

- a crate name and README that describe the generic ptytrace;
- examples that record a tiny CLI session without Docker;
- examples that record a Docker-backed shell session;
- documented guarantees for time virtualization and output source identity;
- a clean separation between reusable modules and consumer scenes;
- stable public names for `PtyTracer`, `PtyTracerConfig`, `ShellProfile`,
  `PresentationOutput`, `Key`, `StubColors`, `Trace`, and frame/verification
  primitives.
