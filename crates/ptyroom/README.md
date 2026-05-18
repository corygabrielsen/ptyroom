# ptyroom

`ptyroom` is the shared-terminal room CLI for trusted local sessions.

One host owns a child PTY and writes a durable `.ptyrecord` bundle
(containing the underlying `.ptytrace`); joined clients see the shared
viewport and can type into the same session. Joined clients use local
`Ctrl-]` controls for detach, help, redraw, and literal prefix input.

```bash
ptyroom host --listen 127.0.0.1:7373 --out /tmp/room.ptyrecord bash
ptyroom join 127.0.0.1:7373
```

The built-in TCP transport has no authentication or encryption. Bind to
loopback and carry remote sessions through SSH, WireGuard, or another
authenticated tunnel.
