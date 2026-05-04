# tint-recorder

Deterministic GIF/MP4 recorder for scripted terminal demos. This repository's
current scenes target [tint](https://github.com/corygabrielsen/tint), but the
recorder core is a reusable PTY driver: it can spawn any interactive argv,
capture terminal IO, and emit asciinema casts whose timestamps come from
virtual presentation time rather than wall-clock recording time.

## Pipeline

```text
scenes/<scene>.rs    Recorder API: forkpty -> process or dockerized shell,
                     scripted input, byte capture, OSC 11/10 responder,
                     deterministic cast timestamps.

src/recording.rs     Recorder-facing trace builder: raw input/output evidence,
                     verified transitions, monotonic timeline, cast artifact.

renderer/snapshot.ts @xterm/headless replays cast -> per-frame JSON.
src/paint.rs         Renders snapshots to PNG with bundled DejaVu.
src/encode.rs        ffmpeg concat-demuxer -> GIF or MP4.
src/verify.rs        Per-scene contract checks rendered snapshots.
```

The reusable crate direction is written down in
[`docs/crate-architecture.md`](docs/crate-architecture.md). The short version:
the recorder core owns PTY IO, virtual time, raw evidence, and cast emission;
tint-specific scenes own theme names, picker targets, and marketing pacing.

Recording is optimized around time virtualization:

- typed spans are sent to bash once, then split into per-character cast events;
- comment/blank/clear presentation is synthesized without a shell round trip;
- synthetic presentation output is marked separately from child PTY output in
  the raw evidence log;
- picker navigation is driven through the real `tint` CLI with terminal
  keypresses, while capture and playback time stay decoupled;
- `TINT_RECORDER_CONTAINER=<name>` reuses a warm container while preserving a
  fresh `$HOME` for every recording.

## Setup

```bash
make setup
```

Host requirements: **`cargo`**, **`docker`**, **Node/npm** for
`@xterm/headless`, and **ffmpeg** for encoding.

## Run

```bash
make recorder-warm       # start/reuse the warm recorder container
make demo-parallel       # fast dev GIF render
make demo-all-parallel   # marketing MP4 + GIF from one capture
make recorder-perf       # isolate startup/typing/prompt/tint/picker legs
make verify              # re-run contract against existing snapshots
cargo run --bin compare_snapshots -- BASELINE_SNAPS CANDIDATE_SNAPS
make clean               # remove generated artifacts
```

`make demo-all-parallel` builds host binaries and the image as needed,
records with the warm container, snapshots, paints once, encodes MP4 and GIF
in parallel, and runs the verify contract. A non-zero exit signals a
regression.

## Layout

| Path                   | Role                                                 | Lang |
| ---------------------- | ---------------------------------------------------- | ---- |
| `Dockerfile`           | Recording image: bash + tint + recorder rcfile       | —    |
| `render-cast.sh`       | Legacy cast → GIF orchestrator                       | bash |
| `Cargo.toml`           | Crate root with strict typing                        | —    |
| `src/color.rs`         | `HexColor`, `CellColor`, palette overrides           | Rust |
| `src/snapshot.rs`      | Snapshot/Cell/Grid with rectangular invariant        | Rust |
| `src/cast.rs`          | asciinema v2 reader/writer                           | Rust |
| `src/paint.rs`         | Renderer (`ab_glyph` + `image`, bundled font)        | Rust |
| `src/encode.rs`        | ffmpeg concat-demuxer wrapper                        | Rust |
| `src/inspect.rs`       | ASCII row dump + ANSI-color row dump                 | Rust |
| `src/verify.rs`        | Contract evaluator                                   | Rust |
| `src/contracts.rs`     | Per-scene contract registry                          | Rust |
| `src/recorder/`        | PTY + shell profile + OSC responder + Recorder API   | Rust |
| `src/recording.rs`     | Raw IO evidence → verified trace → monotonic cast    | Rust |
| `src/proof.rs`         | Typestate markers and invariant scalar types         | Rust |
| `src/raw_log.rs`       | Append-only raw input/output event log               | Rust |
| `src/verified_trace.rs`| Replay-checked semantic transitions                  | Rust |
| `src/proof_timeline.rs`| Verified transitions → deterministic presentation    | Rust |
| `src/scenes.rs`        | Scene helpers and presentation timing knobs          | Rust |
| `src/timeline.rs`      | Presentation beat timing policy                      | Rust |
| `src/bin/recorder_perf.rs` | Capture-leg microbenchmark harness              | Rust |
| `src/bin/compare_snapshots.rs` | Frame-by-frame snapshot A/B comparison      | Rust |
| `scenes/demo_full.rs`  | 4-act marketing demo                                 | Rust |
| `scenes/smoke.rs`      | Minimal smoke scene                                  | Rust |
| `renderer/snapshot.ts` | `@xterm/headless` replay → per-frame JSON            | TS   |
| `assets/fonts/`        | Bundled DejaVu Sans Mono                             | —    |

## Determinism

- Demo commands run inside a pinned `debian:12-slim` image (no host `$HOME` /
  `$PATH` / `.tint` leakage)
- Warm recorder mode uses `docker exec` but creates a fresh `$HOME` per capture
- PTY winsize fixed via `TIOCSWINSZ` before exec
- Recorder answers tint's OSC 11/10 color queries with canned RGB replies,
  so the real `tint` binary runs unmodified
- Cast timestamps come from cumulative `dwell_ms`, never wall-clock
- Proof-backed recorder traces keep raw IO, diagnostic wall time, and playback
  time as separate layers
- Bundled font (`include_bytes!`) ensures identical glyph rasterization
- Unit tests cover total parsers, color algebra, grid invariants, OSC
  query matching, and concat demuxer construction

## Authoring scenes

Scenes are small Rust binaries under `scenes/` that drive a `Recorder`.
For a generic local process, use `Recorder::spawn`:

```rust
use std::time::Duration;
use tint_recorder::recorder::{Recorder, RecorderConfig};

fn main() -> anyhow::Result<()> {
    let mut r = Recorder::spawn(
        RecorderConfig {
            cols: 80, rows: 24, ..Default::default()
        },
        &[
            "env", "-i",
            "HOME=/",
            "TERM=xterm-256color",
            "PS1=$ ",
            "bash", "--noprofile", "--norc", "-i",
        ],
    )?;
    r.send_raw_wait_for(
        &[],
        Duration::ZERO,
        b"$ ",
        Duration::from_secs(2),
        "prompt",
    )?;
    r.type_text("echo hello", Duration::from_millis(35))?;
    r.send_raw_wait_for(
        b"\n",
        Duration::from_millis(300),
        b"$ ",
        Duration::from_secs(2),
        "prompt",
    )?;
    r.push_presentation_output("# done", Duration::from_millis(100))?;
    r.stop()?.write("assets/generic.cast")?;
    Ok(())
}
```

The same example lives in `examples/generic_shell.rs`.

For the Docker-backed tint demo shell, use `Recorder::start`:

```rust
use std::time::Duration;
use tint_recorder::recorder::{Key, Recorder, RecorderConfig};
use tint_recorder::scenes::ms;

fn main() -> anyhow::Result<()> {
    let mut r = Recorder::start(RecorderConfig {
        cols: 80, rows: 30, ..Default::default()
    })?;
    tint_recorder::scenes::wait_for_prompt(&mut r, ms(0), "startup prompt")?;
    r.type_text("tint dracula", ms(50))?;
    r.key(Key::Enter, ms(400))?;
    r.dwell(ms(1000), ms(100))?;
    let cast = r.stop()?;
    cast.write("assets/myscene.cast")?;
    Ok(())
}
```

Scenes run on the host and can either use `Recorder::start` for the
Docker-backed demo shell or `Recorder::spawn` for an arbitrary process. Prefer
content-aware gates (`wait_for_prompt`, `send_raw_wait_for`) over fixed sleeps.
Use virtual presentation helpers only for output that does not affect shell
state, such as comments, blank prompt lines, or a visual clear boundary.

## Why a TypeScript shim

`@xterm/headless` is the only mature terminal emulator with proper OSC 11
support. avt (the asciinema-agg emulator) silently drops OSC. So
`renderer/snapshot.ts` stays in TS — everything else is Rust.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera
license; see `assets/fonts/LICENSE-DejaVu.txt`.
