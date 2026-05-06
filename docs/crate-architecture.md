# Crate Architecture

`tracer` is a reusable deterministic terminal tracer. Its
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

tracer core
  PTY spawn, typed input, raw byte capture, content-aware waits,
  virtual presentation time, asciicast output.

raw evidence and proof layers
  Append-only IO log, verified semantic transitions, monotonic timeline.

terminal model and verification
  Replay casts into snapshots, then check predicates over text, colors,
  cursor-visible state, and forbidden regressions.

rendering
  Convert snapshots into PNG frames, then encode GIF/MP4.
```

## Core Contract

The tracer core owns terminal mechanics:

- process spawning under a PTY;
- terminal rows and columns;
- byte writes;
- byte capture;
- content-aware waits;
- virtual dwell;
- trace construction.

The tracer core must not own product semantics:

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
let mut r = Tracer::spawn(config, &["bash", "-i"])?;
r.type_text("echo hello", char_dwell)?;
r.send_raw_wait_for(b"\n", enter_dwell, b"$ ", timeout, "prompt")?;
r.push_presentation_output("# heading", heading_dwell)?;
let trace = r.stop()?;
```

The Docker convenience path is an adapter:

```rust
let mut r = Tracer::start(TracerConfig {
    shell: ShellProfile::simple(),
    ..TracerConfig::default()
})?;
```

Consumer-specific script helpers should remain outside the tracer core.

## Verification Model

A recording is not "good" because encoding succeeded. It is good when:

- raw IO is closed;
- expected transitions are replay-verified;
- presentation timestamps are monotonic;
- rendered snapshots satisfy the script contract.

The verification layer should be reusable. A consumer should be able to assert
that text appeared, that text never appeared, that a color was reached, or that
the final frame matches a desired loop state.

## Publication Bar

Before publishing, this project should have:

- a crate name and README that describe the generic tracer;
- examples that record a tiny CLI session without Docker;
- examples that record a Docker-backed shell session;
- documented guarantees for time virtualization and output source identity;
- a clean separation between reusable modules and consumer scenes;
- stable public names for `Tracer`, `TracerConfig`, `ShellProfile`,
  `PresentationOutput`, `Key`, `StubColors`, `Trace`, and frame/verification
  primitives.
