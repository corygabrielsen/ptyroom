# Shared Terminals

`ptyroom` is the shared-terminal command. It exposes explicit `host`,
`join`, and `watch` operations:

```bash
ptyroom host \
    --listen 127.0.0.1:7373 \
    --out /tmp/room.ptytrace \
    --cols 100 \
    --rows 30 \
    bash

ptyroom join 127.0.0.1:7373
ptyroom watch 127.0.0.1:7373
```

The host terminal participates by default. That means local host typing
goes into the shared PTY and local host output is rendered in the host
terminal. Use `--no-local-input` when the host should observe while
joined clients drive the session. Use `--no-local-output` when the host
process should run as a headless relay.

When the host terminal is an interactive tty and local output is
enabled, `ptyroom host` enters an alt-screen viewport with a `[HOST]`
status row at the bottom showing the bound address, the child command,
and the live client count. Piped host stdout and `--no-local-output`
keep the old pass-through behavior so non-interactive pipelines are
unchanged.

`ptyroom watch` is a read-only client: it receives the same broadcast
output as a join, but never forwards local input bytes and never reports
its terminal size. A watcher cannot type into the PTY and cannot shrink
the shared canvas, no matter how small its window is.

## Local Demo

The simplest demo uses three terminals on one machine.

Terminal 1:

```bash
ptyroom host \
    --listen 127.0.0.1:7373 \
    --out /tmp/ptyroom-demo.ptytrace \
    --cols 100 \
    --rows 30 \
    --no-local-input \
    bash
```

Terminals 2 and 3:

```bash
ptyroom join 127.0.0.1:7373
```

Both joined terminals are now typing into the same shell. Run ordinary
terminal programs: `bash`, `vim`, `htop`, a REPL, or any CLI that behaves
inside a PTY. End the child shell with `exit`, or detach an individual
join client with `Ctrl-] .`.

When the host exits, the trace path from `--out` can be rendered:

```bash
ptyrender /tmp/ptyroom-demo.ptytrace room.gif
```

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

There is no separate edit log and no per-client cursor in the current
model. The room is intentionally a shared PTY, not a collaborative text
editor. If two people type at once, their bytes race into the same child
process in the order the host event loop reads them.

Late joiners receive the current terminal size followed by a bounded
replay of recent complete output frames before live output resumes. The
replay buffer evicts whole frames, so a client never starts in the middle
of a length-delimited payload.

Slow clients have bounded output backlogs. If a client stops reading and
exceeds the backlog limit, that client is disconnected instead of
stalling the PTY owner, recorder, or other clients.

## Join-Local Controls

When both stdin and stdout are terminals, `ptyroom join` reserves
`Ctrl-]` as a local prefix. The prefix is handled by the join process
before bytes are sent to the room:

- `Ctrl-] .` detaches this join client.
- `Ctrl-] ?` shows local help.
- `Ctrl-] r` redraws the local viewport.
- `Ctrl-] Ctrl-]` sends a literal `Ctrl-]` into the shared PTY.

All other control bytes, including bare `Ctrl-C`, `Esc`, and `q`, remain
remote input. This keeps full-screen programs usable while still giving
the join client a local escape hatch.

Interactive clients also reserve one local status row. That row belongs
to the join process, not the shared PTY, and is excluded from the size the
client reports to the host.

Piped input does not install local controls, but it can still render in
the local viewport when stdout is a terminal. If stdout is not a
terminal, `ptyroom join` behaves like a transport filter: stdin bytes go
to the room, protocol controls are decoded, and raw PTY output bytes go
to stdout.

`ptyroom watch` reuses the same viewport and local-control machinery, but
drops every byte that would otherwise be sent to the room. Local stdin is
still read so that `Ctrl-]` detach, redraw, and help continue to work;
the `Ctrl-] Ctrl-]` "send literal" affordance is removed because there is
no upstream input channel. The status line identifies the mode as
`ptyroom watch <addr> | read-only`.

## Geometry

`ptyroom host` owns one canonical child PTY size. Rendering participants
report terminal size:

- the host terminal, when local output is enabled;
- interactive `ptyroom join` clients.

When the host has its own alt-screen viewport, the host reports its
local terminal size minus one reserved status row, mirroring the rule
that applies to interactive join clients. Piped or `--no-local-output`
host stdout still reports the full terminal size since no status row is
drawn.

`ptyroom watch` clients are deliberately excluded from this calculation.
A watcher's window size has no effect on the shared PTY, so a small
observer cannot shrink the canvas seen by everyone else.

The canonical size is the smallest known rendering terminal. This is the
tmux-like rule that prevents a zoomed-in participant from seeing a wider
logical terminal than it can display. Larger terminals render the shared
canvas into their alternate screen and leave unused space blank. When the
smallest participant disconnects, the room can grow to the next smallest
active renderer. Resize changes are recorded as asciicast resize events.

Non-terminal clients keep pipeline behavior: controls are stripped and
decoded PTY output bytes are written to stdout.

## Common Failure Modes

Nothing happens after typing on the host:

- If the host was started with `--no-local-input`, host keystrokes are
  intentionally ignored. Type from a joined client or restart without
  `--no-local-input`.
- If the child program is waiting for input without echoing, verify from
  a second terminal with `ptyroom join <addr>`.

The joined terminal looks clipped or has blank space:

- This is expected when participants have different window sizes. The
  room chooses the smallest active rendering size. Larger terminals show
  the shared canvas plus unused space.
- Resize the smallest terminal or detach it to let the room grow.

The cursor or alternate screen did not restore:

- Normal exits and catchable termination signals run cleanup guards.
- `SIGKILL`, `SIGSTOP`, terminal emulator crashes, and OS failures cannot
  run cleanup. Run `reset` in the affected terminal if that happens.

Remote connection is refused:

- The default listen address is loopback. Use SSH port forwarding or
  another authenticated tunnel for remote participants.
- Non-loopback binds require
  `--allow-unauthenticated-public-bind`; only use that behind a trusted
  network boundary.

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
