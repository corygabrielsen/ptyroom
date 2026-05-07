# Shared Terminals

`ptyroom` is the shared-terminal command. It exposes explicit `host` and
`join` operations:

```bash
ptyroom host \
    --listen 127.0.0.1:7373 \
    --out /tmp/room.ptytrace \
    --cols 100 \
    --rows 30 \
    bash

ptyroom join 127.0.0.1:7373
```

The host terminal participates by default. That means local host typing
goes into the shared PTY and local host output is rendered in the host
terminal. Use `--no-local-input` when the host should observe while
joined clients drive the session. Use `--no-local-output` when the host
process should run as a headless relay.

## Layers

```text
ptyroom host ── pty::share ── child PTY ── .ptytrace
                                   │
                                   └── framed output to clients

ptyroom join ── pty::connect ── local terminal viewport or stdout
```

- `ptyroom` is the stable shared-terminal command.
- `pty::share` hosts one shared PTY over TCP.
- `pty::connect` attaches a local terminal to an existing room.

## Session Model

One host process owns one child PTY. Host stdin and connected client
input bytes are interleaved into that PTY. PTY output is length-framed,
broadcast to all clients, and recorded as `.ptytrace` output events.

Late joiners receive the current terminal size followed by a bounded
replay of recent complete output frames before live output resumes. The
replay buffer evicts whole frames, so a client never starts in the middle
of a length-delimited payload.

Slow clients have bounded output backlogs. If a client stops reading and
exceeds the backlog limit, that client is disconnected instead of
stalling the PTY owner, recorder, or other clients.

## Geometry

`ptyroom host` owns one canonical child PTY size. Rendering participants
report terminal size:

- the host terminal, when local output is enabled;
- interactive `ptyroom join` clients.

The canonical size is the smallest known rendering terminal. This is the
tmux-like rule that prevents a zoomed-in participant from seeing a wider
logical terminal than it can display. Larger terminals render the shared
canvas into their alternate screen and leave unused space blank. When the
smallest participant disconnects, the room can grow to the next smallest
active renderer. Resize changes are recorded as asciicast resize events.

Non-terminal clients keep pipeline behavior: controls are stripped and
decoded PTY output bytes are written to stdout.

## Terminal Cleanup

Interactive frontends install a best-effort cleanup guard for terminal
visual state. On normal exit and catchable termination signals, the guard
restores common transient modes, exits the alternate screen, and shows
the cursor. The signal path covers `SIGINT`, `SIGTERM`, `SIGHUP`, and
`SIGQUIT`.

No process can clean up after `SIGKILL`, `SIGSTOP`, a terminal emulator
crash, or an OS-level failure that prevents the process from running
destructors or signal handlers.

## Security Boundary

The shared-terminal transport has no authentication, authorization,
encryption, or replay protection. A connected client can type into the
PTY.

Defaults are conservative: listeners bind to loopback by default, and
non-loopback binds require `--allow-unauthenticated-public-bind`. For
remote use, put the TCP stream behind SSH, WireGuard, a private overlay
network, or another authenticated tunnel.

The raw byte protocol is documented in
[`ptyroom-protocol.md`](ptyroom-protocol.md).
