# Determinism audit

The render pipeline must produce byte-identical output bytes given
identical inputs. This is the property `Receipt::verify` proves
empirically on every check; this document records the per-layer
inspection that justifies the claim.

Audit scope: `cast → output` (the right arrow). The `scene → cast`
arrow (recording) is wall-clock dependent by design; see
`docs/scene-grammar.md` for why scene_sha256 is provenance only.

## Layer-by-layer

### Cast parser (`src/cast.rs`)

- Pure JSON parse via `serde_json`.
- `Cast.events: Vec<CastEvent>` — order-preserving collection.
- `CastHeader.env: BTreeMap<String, String>` — sorted key
  serialization (not HashMap).
- Output: deterministic given input bytes.

### Snapshot replay (`src/snapshot_replay/`)

- `vt100::Parser` is a state machine over input bytes; no time, no
  randomness.
- `OscTracker.palette: BTreeMap<u8, HexColor>` — sorted iteration
  for `palette_overrides()`.
- `replay()` iterates `cast.events` by index in order; one snapshot
  per `Output` event in that order.
- Snapshot frame names use the cast event index (`format!("{:04}",
i + 1)`) — preserves a stable lexicographic ordering for
  downstream concat.
- `dwell_ms` rounding clamps to `[1, u32::MAX]`; deterministic given
  input timestamps.
- No HashMap usage anywhere in the module.
- Output: deterministic given cast bytes + `StubColors`.

### Paint (`src/paint.rs`)

- `Painter::paint(snapshot) -> RgbImage` — pure given font bytes +
  paint config.
- `paint_is_byte_stable` test asserts: two paints of the same
  snapshot produce identical pixel buffers. Live regression gate.
- Parallelism (`par_iter` in `src/render.rs:execute` and
  `src/bin/term-recorder/paint.rs`): each thread paints one
  snapshot to one PNG file. Snapshots are independent; PNG file
  paths derive from `entry.frame` (deterministic). No shared
  mutable state.
- PNG encoding via `image` crate uses zlib with default level —
  deterministic given identical input pixel buffer.
- Output: deterministic given snapshot + font bytes.

### Encode (`src/encode.rs`)

ffmpeg invocation flags are explicitly pinned for byte-stable output.

**`encode_mp4` (libx264 path):**

| Flag                                                    | Value            | Determinism reason                                                                                                                           |
| ------------------------------------------------------- | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `-c:v libx264`                                          | software encoder | no GPU/driver variance                                                                                                                       |
| `-crf 20`                                               | constant quality | no rate-control randomness                                                                                                                   |
| `-preset medium`                                        | pinned           | preset selection affects partitioning                                                                                                        |
| `-tune stillimage`                                      | pinned           | tuning affects encoder decisions                                                                                                             |
| `-threads 1`                                            | **pinned**       | "the only way to get byte-stable output across runs. Multi-threaded x264 partitions slices nondeterministically." (source comment, line 187) |
| `-profile:v high -level 4.0`                            | pinned           | codec profile lock                                                                                                                           |
| `-movflags +faststart`                                  | pinned           | MOOV atom placement upfront                                                                                                                  |
| `-vf "fps=N,format=yuv420p[,scale=W:-2:flags=lanczos]"` | pinned           | filter chain order matters                                                                                                                   |

The `-threads 1` flag is the load-bearing knob. Multi-threaded
libx264 is a well-known source of nondeterminism (slice partition
boundaries depend on thread scheduling). Comment in source shows
the author understood this and pinned it.

**`encode_gif` path:**

| Flag                                    | Value  | Determinism reason                                                                 |
| --------------------------------------- | ------ | ---------------------------------------------------------------------------------- |
| `palettegen=stats_mode=full`            | pinned | full statistical pass, not sampled                                                 |
| `paletteuse=dither=bayer:bayer_scale=5` | pinned | Bayer dither is deterministic; error-diffusion alternatives can be order-dependent |
| `-loop 0`                               | pinned | infinite loop                                                                      |

GIF path is deterministic by construction.

**`encode_mp4` (h264_nvenc path):**

The `Mp4Encoder::H264Nvenc` variant is **explicitly non-deterministic**:

> NVIDIA NVENC hardware H.264 encoder. Faster wall-time but requires
> a CUDA-capable GPU + matching ffmpeg build, **and is not bit-for-
> bit reproducible across driver versions.**
> — `src/encode.rs:34-38`

The receipt's `RenderOptions.mp4_encoder` field captures which
encoder was used, so a verifier sees the choice. **For blockchain
provenance, NVENC-encoded artifacts are NOT byte-reproducible
across machines** and should not be attested.

Gap: `Receipt::verify` does not refuse NVENC receipts. It will
re-render with NVENC and then output_sha256 won't match — so the
verify call returns `OutputDiffers`, but the failure mode looks
like "the receipt is broken" rather than "this encoder isn't
verifiable." Worth a future doc clarification or refusal.

### Concat file (`src/encode.rs:build_concat`)

- Iterates `timing` in order (input order from snapshot stage).
- Writes absolute paths via `frames_dir.canonicalize()`.
- Last frame repeated (ffmpeg concat demuxer quirk).
- Per-call tempfile path (no concurrent-encode race on shared concat).
- Deterministic given timing list.

### Cast file write (`src/cast.rs`)

- `Cast.to_string()` writes header line + events line by line.
- Each event serialized via `serde_json::to_string` — stable for
  sequence types.
- Header's `env: BTreeMap` ensures sorted key order.
- Output: deterministic given Cast struct.

## Tool identity

The receipt's `tool` field captures the dependencies that, if
changed, can produce different output bytes:

```rust
pub struct ToolIdentity {
    pub name: String,            // "term-recorder"
    pub version: String,         // CARGO_PKG_VERSION
    pub ffmpeg_version: String,  // first line of `ffmpeg -version`
    pub font_sha256: String,     // hash of bundled DejaVu Sans Mono
}
```

**Adequacy:** these four pin the major variance sources. ffmpeg
version captures libx264 + libfreetype + container muxer behavior;
font hash captures glyph rasterization input; recorder name+version
capture the rest of the Rust side.

**Gap (for strict blockchain attestation):** the recorder version
corresponds to a `Cargo.toml` version string, not a specific binary
hash. Two builds of the recorder at version `0.1.0` from different
`Cargo.lock` states (different vt100 / image / serde_json patch
versions) could in principle produce different output. The tool
identity does not currently include the recorder binary's own
SHA-256 or the `Cargo.lock` hash.

For most use cases this is fine — patch-level dep variance
typically doesn't affect output. For blockchain trust where any
divergence breaks consensus, recommend adding either:

1. `recorder_sha256` field — hash of the binary at recording time,
   verifiable on any node by hashing its own binary.
2. `cargo_lock_sha256` field — hash of the Cargo.lock used to build
   the recorder, narrower but more portable.

Either change is additive (use `#[serde(default,
skip_serializing_if)]` like `spec_sha256` and `scene_sha256`).

## Empirical confirmation

These layer-level claims are continuously verified by:

- `cargo test --lib` (131 tests including `paint_is_byte_stable`)
- `make verify-goldens` (45 layer hashes across 5 scenes)
- `make bless-goldens` (N=10 agreement gate; refuses to commit a
  golden if any layer disagrees across 10 runs)
- `Receipt::verify` (re-renders the cast and compares output hash;
  runs on every receipt check)

The cli scene migration (`9c1a98d` in tint-scenes) is a worked
example: a DSL-driven cast and an .rs-driven cast of the same
scene produce byte-identical output (same SHA-256 across N=10),
confirming that determinism survives the .rs → .scene boundary.

## Summary

| Layer             | Determinism source                      | Verified     |
| ----------------- | --------------------------------------- | ------------ |
| Cast parse        | `serde_json` + `Vec`/`BTreeMap`         | yes          |
| Snapshot replay   | vt100 state machine + `BTreeMap`        | yes          |
| Paint             | pure rasterization, byte-stability test | yes (test)   |
| Encode (libx264)  | pinned ffmpeg flags incl. `-threads 1`  | yes          |
| Encode (NVENC)    | **non-deterministic by design**         | n/a (opt-in) |
| Encode (GIF)      | pinned palettegen + Bayer dither        | yes          |
| Concat file       | deterministic input ordering            | yes          |
| Cast write        | sorted env, ordered events              | yes          |
| **Tool identity** | **partial — see "Gap" above**           | **partial**  |

Every layer except tool identity has a strong determinism story.
The libx264 path, GIF path, snapshot replay, paint, and cast
serialization are all byte-stable by construction or by test.
NVENC is opt-in and explicitly flagged non-portable. Tool identity
captures version + ffmpeg + font but not the recorder binary
itself; the recommended additive field for strict provenance is
`recorder_sha256` in the receipt.
