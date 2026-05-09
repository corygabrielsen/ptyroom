# ptyrecord

`ptyrecord` composes trace capture and rendering into portable
`.ptyrecord` bundles.

A bundle contains the trace, rendered media, witness data, and a
selectable-text projection for playback UIs. The embedded `.ptytrace`
remains the source of truth; media and transcripts are derived views.

```bash
ptyrecord --out demo.ptyrecord bash
ptyrecord --trace-in demo.ptytrace \
    --media-in demo.mp4 \
    --witness-in demo.mp4.witness.json \
    --out demo.ptyrecord
```

Use `ptytrace` when you only need the raw trace and `ptyrender` when you
only need standalone media.
