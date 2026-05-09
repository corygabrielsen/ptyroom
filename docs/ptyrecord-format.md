# `.ptyrecord` Format

`.ptyrecord` is the portable playback bundle produced by `ptyrecord`.
It is JSON so browser components can load it with `fetch(...).json()`
without a decompression dependency.

## Algebra

```text
ptytrace(command)  -> .ptytrace
ptyrender(trace)   -> media (.gif or .mp4)
ptyrecord(command) -> .ptyrecord
```

`ptyrecord(command)` is the composition:

```text
ptyrecord(command) = bundle(ptytrace(command), ptyrender(ptytrace(command)))
```

For build pipelines that already have a trace and a rendered MP4,
`ptyrecord --trace-in T --media-in M --witness-in W --out R` runs only
the `bundle(...)` step.

The bundle is not the source of truth for terminal behavior; the embedded
`.ptytrace` is. Media and selectable text are derived projections.
Schema v1 embeds MP4 media so browser components can provide native
playback controls and time-synchronized selectable text. Use `ptyrender`
directly when you need a standalone GIF.

During live command recording, `ptyrecord` feeds each captured output
event through the same replay state as `ptyrender` and paints PNG frames
immediately. At session end it encodes the already-painted frame set,
then computes the witness from the trace and output hashes without
re-rendering the terminal session.

## Schema v1

```json
{
  "version": 1,
  "trace": {
    "path": "demo.ptytrace",
    "media_type": "application/x-ptytrace+jsonl",
    "sha256": "...",
    "encoding": "base64",
    "bytes_base64": "..."
  },
  "media": {
    "path": "demo.mp4",
    "media_type": "video/mp4",
    "sha256": "...",
    "encoding": "base64",
    "bytes_base64": "..."
  },
  "witness": {},
  "transcript": {
    "plain_text": "selectable copy/search text",
    "frames": [
      { "time_s": 0.0, "rows": ["visible terminal row"] }
    ]
  }
}
```

`witness` is optional. When present, it is the same render witness
produced by `ptyrender trace.ptytrace out.mp4 --receipt witness.json`.

## Text Projection

`transcript.frames` is derived by replaying the trace with the same VT100
pipeline used by the renderer. Each item contains the terminal rows
visible after an output event, keyed by trace time. UI components can
select the latest frame whose `time_s <= media.currentTime`.

`transcript.plain_text` strips ANSI/OSC controls from output events and
preserves printable text. It is a copy/search convenience, not a
verification input.

## Invariants

- `trace.bytes_base64` decodes to the exact `.ptytrace` bytes whose hash
  is `trace.sha256`.
- `media.bytes_base64` decodes to the exact media bytes whose hash is
  `media.sha256`.
- `witness.trace_sha256`, when present, equals `trace.sha256`.
- `witness.output_sha256`, when present, equals `media.sha256`.
- Text projections are derived from the embedded trace; they are never
  manually authored.
