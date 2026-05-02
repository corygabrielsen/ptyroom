# tint-recorder

Deterministic GIF recorder for [tint](https://github.com/corygabrielsen/tint)
demos. The pipeline is Rust + a tiny TypeScript shim for the one piece that
needs `@xterm/headless` (terminal emulation). Demos run inside a pinned
Docker image so the bytes you record on your machine are the bytes anyone
gets running the same scene.

## Pipeline

```
scenes/<scene>.rs    Recorder API — pty.fork → docker run → bash + tint,
                     scripted keystrokes, byte capture, OSC 11/10 query
                     responder, deterministic cast timestamps.

render-cast.sh       In-container orchestrator:
  renderer/          @xterm/headless replays cast → per-frame JSON   (TS)
    snapshot.ts
  src/paint.rs       Renders each snapshot to PNG (bundled DejaVu)   (Rust)
  src/encode.rs      ffmpeg concat-demuxer → final GIF              (Rust)
  src/verify.rs      Per-scene contract checks the result            (Rust)
```

## Setup

```bash
make setup
```

Host requirements: **`cargo`** (compile scene binaries) and **`docker`**
(everything else). No Python or Node on the host.

## Run

```bash
make build-image  # builds the demo container (once, then on tint changes)
make demo         # the polished 4-act marketing demo
make smoke        # minimal smoke scene
make verify       # re-run the contract against existing snapshots
make clean        # remove generated artifacts
```

`make demo` builds host binaries and the image as needed, records the
cast, renders the GIF inside the container, and runs the verify contract.
A non-zero exit signals a regression.

## Layout

| Path                   | Role                                                 | Lang |
| ---------------------- | ---------------------------------------------------- | ---- |
| `Dockerfile`           | Multi-stage: Rust builder + slim runtime             | —    |
| `render-cast.sh`       | In-container cast → GIF orchestrator                 | bash |
| `Cargo.toml`           | Crate root with strict typing                        | —    |
| `src/color.rs`         | `HexColor`, `CellColor`, palette overrides           | Rust |
| `src/snapshot.rs`      | Snapshot/Cell/Grid with rectangular invariant        | Rust |
| `src/cast.rs`          | asciinema v2 reader/writer                           | Rust |
| `src/paint.rs`         | Renderer (`ab_glyph` + `image`, bundled font)        | Rust |
| `src/encode.rs`        | ffmpeg concat-demuxer wrapper                        | Rust |
| `src/inspect.rs`       | ASCII row dump + ANSI-color row dump                 | Rust |
| `src/verify.rs`        | Contract evaluator                                   | Rust |
| `src/contracts.rs`     | Per-scene contract registry                          | Rust |
| `src/recorder/`        | PTY + OSC responder + Recorder API                   | Rust |
| `src/scenes.rs`        | Scene helpers (`line`, `blank`, `lookup_picker_idx`) | Rust |
| `scenes/demo_full.rs`  | 4-act marketing demo                                 | Rust |
| `scenes/smoke.rs`      | Minimal smoke scene                                  | Rust |
| `renderer/snapshot.ts` | `@xterm/headless` replay → per-frame JSON            | TS   |
| `assets/fonts/`        | Bundled DejaVu Sans Mono                             | —    |

## Determinism

- Demo runs inside a pinned `debian:12-slim` image (no host `$HOME` /
  `$PATH` / `.tint` leakage)
- PTY winsize fixed via `TIOCSWINSZ` before exec
- Recorder answers tint's OSC 11/10 color queries with canned RGB replies,
  so the real `tint` binary runs unmodified
- Cast timestamps come from cumulative `dwell_ms`, never wall-clock
- Bundled font (`include_bytes!`) ensures identical glyph rasterization
- 58 unit tests cover total parsers, color algebra, grid invariants, OSC
  query matching, and concat demuxer construction

## Authoring scenes

Scenes are small Rust binaries under `scenes/` that drive a `Recorder`:

```rust
use std::time::Duration;
use tint_recorder::recorder::{Key, Recorder, RecorderConfig};
use tint_recorder::scenes::ms;

fn main() -> anyhow::Result<()> {
    let mut r = Recorder::start(RecorderConfig {
        cols: 80, rows: 30, ..Default::default()
    })?;
    r.dwell(ms(800), ms(600))?;
    r.type_text("tint dracula", ms(35))?;
    r.key(Key::Enter, ms(400))?;
    r.dwell(ms(1200), ms(100))?;
    let cast = r.stop()?;
    cast.write("assets/myscene.cast")?;
    Ok(())
}
```

Scenes run on the host and spawn the bash session inside the container.
Every directory, `.tint`, or `.theme` the demo references must be created
on screen during the recording (`mkdir`, `echo > .tint`, heredoc) — no
magic pre-prepared state.

## Why a TypeScript shim

`@xterm/headless` is the only mature terminal emulator with proper OSC 11
support. avt (the asciinema-agg emulator) silently drops OSC. So
`renderer/snapshot.ts` stays in TS — everything else is Rust.

## License

MIT — see `LICENSE`. Bundled DejaVu Sans Mono is under the Bitstream Vera
license; see `assets/fonts/LICENSE-DejaVu.txt`.
