# Crate Architecture

This project is being shaped as a reusable deterministic terminal recorder.
The tint demo is the first serious consumer, not the abstraction boundary.

## Design Goal

Record scripted terminal sessions as reproducible artifacts:

- capture real PTY input and output as raw evidence;
- keep playback time independent from wall-clock capture time;
- allow presentation-only bytes, but mark them as synthetic;
- verify the rendered terminal state before treating a recording as good;
- render the same cast into MP4, GIF, snapshots, or test fixtures.

The core should be useful for any CLI tool. Product-specific scenes should
live above it.

## Layers

```text
app scenes
  Domain story: commands, labels, timing taste, expected states.

scene helpers
  Small reusable beats: type a line, press Enter, wait for a prompt,
  hold a frame, add presentation-only text.

recorder core
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

The recorder core owns terminal mechanics:

- process spawning under a PTY;
- terminal rows and columns;
- byte writes;
- byte capture;
- content-aware waits;
- virtual dwell;
- cast construction.

The recorder core must not own product semantics:

- no theme names;
- no picker targets;
- no product-specific file names;
- no app-specific shortcuts hidden below the scene layer.

## Time Model

There are two clocks.

Wall-clock time is diagnostic and synchronization-only. It answers:

- has the child emitted the prompt yet?
- has the alternate screen appeared?
- did a command hang?

Presentation time is the cast timeline. It answers:

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
presentation_output  bytes inserted by the scene for visual structure
```

Both `output` and `presentation_output` render into the final cast.
Only `output` came from the child process.

This distinction is non-negotiable. It lets scenes use labels and visual
spacing without pretending those bytes were produced by the CLI being
demonstrated.

## Public API Shape

The generic API should stay small:

```rust
let mut r = Recorder::spawn(config, &["bash", "-i"])?;
r.type_text("echo hello", char_dwell)?;
r.send_raw_wait_for(b"\n", enter_dwell, b"$ ", timeout, "prompt")?;
r.push_presentation_output("# heading", heading_dwell)?;
let cast = r.stop()?;
```

The Docker convenience path is an adapter:

```rust
let mut r = Recorder::start(RecorderConfig {
    shell: ShellProfile::simple(),
    ..RecorderConfig::default()
})?;
```

Tint-specific scene helpers should remain outside the recorder core.

## Verification Model

A recording is not "good" because encoding succeeded. It is good when:

- raw IO is closed;
- expected transitions are replay-verified;
- presentation timestamps are monotonic;
- rendered snapshots satisfy the scene contract.

The verification layer should be reusable. A consumer should be able to assert
that text appeared, that text never appeared, that a color was reached, or that
the final frame matches a desired loop state.

## Publication Bar

Before publishing, this project should have:

- a crate name and README that describe the generic recorder, not only tint;
- examples that record a tiny generic CLI session without Docker;
- examples that record a Docker-backed shell session;
- documented guarantees for time virtualization and output source identity;
- a clean separation between reusable modules and tint demo scenes;
- stable public names for `Recorder`, `RecorderConfig`, `ShellProfile`,
  `PresentationOutput`, `Key`, `StubColors`, `Cast`, and snapshot/verification
  primitives.

Until then, tint remains the proving ground.
