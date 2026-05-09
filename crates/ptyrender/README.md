# ptyrender

`ptyrender` turns `.ptytrace` files into deterministic GIF/MP4 media and
render witnesses.

It owns replaying trace bytes into frames, painting frames with the
bundled font, encoding media, and verifying that a witness reproduces
the same output from the same trace and render identity.

```bash
ptyrender demo.ptytrace demo.gif
ptyrender demo.ptytrace demo.mp4 --receipt demo.mp4.witness.json
ptyrender verify --witness demo.mp4.witness.json --trace demo.ptytrace
```

Library entry points include `ptyrender::render`,
`ptyrender::frame_replay`, `ptyrender::paint`, `ptyrender::encode`, and
`ptyrender::witness`.
