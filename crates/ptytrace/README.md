# ptytrace

`ptytrace` is the PTY trace crate and raw recorder CLI.

It records interactive terminal sessions into durable `.ptytrace` files,
runs reproducible `.script` recordings, stitches traces, writes trace
attestations, and checks behavioral contracts over traces. It does not
render media; use `ptyrender` for GIF/MP4 output and render witnesses.

```bash
ptytrace htop
ptytrace capture --out demo.ptytrace
ptytrace run demo.script --out demo.ptytrace
ptytrace check --trace demo.ptytrace --contract demo.contract.json
```

Library entry points include `ptytrace::trace`, `ptytrace::pty`,
`ptytrace::script`, `ptytrace::contract`, and `ptytrace::attestation`.
