# `ptyshare` Protocol

`ptyshare` is the first collaborative-terminal transport in this crate.
It is intentionally small: one process owns one PTY, peers connect over
TCP, peer bytes go into the PTY, and PTY output bytes go back to every
connected peer while the host records the same output as `.ptytrace`.

## Topology

```text
host stdin ─┐
client A ───┼──> ptyshare ──> child PTY ──> broadcast output ──> clients
client B ───┘                    │
                                  └──> .ptytrace
```

The host terminal is a participant by default. Use `--no-local-input`
when `ptyshare` should be a headless relay driven only by clients.

## Byte Streams

Client-to-host is a raw terminal input byte stream with reserved
`ptyshare` DCS control frames for terminal geometry.

Host-to-client is a framed stream. Control frames still use the reserved
`ptyshare` DCS namespace, but PTY output is sent as length-delimited data
so child output can contain arbitrary escape sequences without being
mistaken for trusted transport control:

```text
ESC P ptyshare;data;<byte-len> ESC \ <byte-len raw PTY output bytes>
```

- Client to host: raw terminal input bytes, except reserved
  `ptyshare` DCS control frames.
- Host to client: `ptyshare` DCS control frames plus length-delimited
  data frames carrying raw PTY output bytes.
- Recording: PTY output bytes as `.ptytrace` output events, with
  `.ptytrace` resize events whenever the canonical PTY size changes.

When a client joins, `ptyshare` first sends the current size control
frame, then a bounded replay of recent data frames. This keeps late
joiners from seeing a blank terminal until the next output event while
preserving the same framing rules as live output.

This means `ptyconnect` can be simple but not byte-blind: relay stdin to
the socket, parse host-to-client frames, and write only decoded PTY output
bytes to stdout. After piped stdin closes, it half-closes the socket write
side and continues reading output until `ptyshare` closes.

## Terminal Geometry

`ptyshare` owns one canonical PTY size. `ptyconnect` reports its local
terminal size using a reserved client-to-server DCS control frame:

```text
ESC P ptyshare;resize;<cols>;<rows> ESC \
```

`ptyshare` strips this frame from the input stream, records the client's
latest size, and resizes the child PTY to the smallest known attached
rendering terminal size. The host terminal also participates in this
calculation when local output is enabled and the host stdout is a
terminal. If no rendering terminal has reported a size, `ptyshare` uses
the configured initial size.

Whenever a client joins or the canonical size changes, `ptyshare` sends a
reserved server-to-client DCS control frame:

```text
ESC P ptyshare;size;<cols>;<rows> ESC \
```

Interactive `ptyconnect` clients use that frame to resize a local
`vt100` screen model. They render the model into the local terminal's
alternate screen, clearing unused space so larger terminals see a stable
canvas instead of stale raw-output artifacts. Non-terminal stdout keeps
pipeline behavior: `ptyconnect` strips `ptyshare` control frames and
writes raw PTY output bytes.

This is the first tmux-like geometry rule: no connected rendering
`ptyconnect` client should be smaller than the logical PTY it is
rendering. Larger clients get blank space outside the canonical shared
screen, and removing the smallest client allows the session to grow to
the next smallest active renderer.

## Scheduling

`ptyshare` polls four classes of file descriptor:

- listener socket for new clients;
- host stdin, when local input is enabled;
- the PTY master;
- connected client sockets.

Input ordering is the order in which the host event loop drains readable
participants. There is no causal metadata or per-client identity in the
trace yet. If identity matters, put an authenticated layer in front of
the TCP stream and attach that identity through an attestation.

## Backpressure

Client output is nonblocking. Each client has a bounded output backlog,
including any replay bytes queued when it joins.
If a client stops reading and its backlog exceeds the limit, `ptyshare`
disconnects that client instead of blocking:

- the PTY owner;
- the recorder;
- other clients.

This is the core progress invariant for the current transport: a slow
observer can lose its session, but it cannot stall the shared session.

## Lifecycle

- The listener is nonblocking and accepts all pending clients whenever it
  becomes readable.
- Client EOF or socket errors remove that client.
- PTY EOF, PTY error, or `--max-secs` ends the session.
- At session end, `ptyshare` terminates the child process and writes the
  trace.

The CLI summary reports accepted clients, disconnected clients, and
backlog drops. Backlog drops are also counted as disconnects.

## Security Boundary

`ptyshare` has no authentication, authorization, encryption, or replay
protection. A connected client can type into the PTY.

Defaults are conservative:

- `--listen` defaults to loopback.
- Non-loopback binds are refused unless
  `--allow-unauthenticated-public-bind` is present.

For remote use, run `ptyshare` behind SSH, WireGuard, a private overlay
network, or another authenticated transport. In the algebra of the
system, `ptyshare` is only the byte transport; identity is a separate
provenance or attestation layer.

## Non-Goals For This Protocol Version

- Per-client identity in the trace.
- Encrypted transport.
- Multi-host conflict resolution.
- Replayable client input logs.
- Rich per-client status bars or scrollback.
- File transfer or side channels.

Those are compatible future layers, but they are not part of the current
raw stream contract.
