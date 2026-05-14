# `ptyroom` Protocol

`ptyroom` is intentionally small: one host process owns one PTY, peers
join over TCP, peer bytes go into the PTY, and PTY output bytes go back
to every connected peer while the host records the same output as
`.ptytrace`. For command choice, terminal geometry behavior, and operator
guidance, see [`shared-terminals.md`](shared-terminals.md).

## Topology

```text
host stdin ─┐
client A ───┼──> ptyroom host ──> child PTY ──> broadcast output ──> clients
client B ───┘                         │
                                       └──> .ptytrace
```

The host terminal is a participant by default. Use `--no-local-input`
when `ptyroom host` should be a headless relay driven only by clients.

## Byte Streams

Both directions begin with a required protocol hello:

```text
ESC P ptyroom;hello;1 ESC \
```

A peer that receives any data or geometry frame before a supported hello
must fail the connection. This makes mixed-version rooms fail clearly
instead of treating old control bytes as terminal content.

## Frame Reference

All control frames use the DCS namespace:

```text
ESC P ptyroom;<payload> ESC \
```

Payloads:

| Direction      | Payload                                 | Meaning                                                                  |
| -------------- | --------------------------------------- | ------------------------------------------------------------------------ |
| both           | `hello;1`                               | Protocol version handshake. Must appear before any other trusted frame.  |
| client -> host | `resize;<cols>;<rows>`                  | Latest rendering size for that join client. Zero dimensions are invalid. |
| host -> client | `size;<cols>;<rows>`                    | Canonical shared PTY size chosen by the host.                            |
| host -> client | `data;<byte-len>` followed by raw bytes | Length-delimited PTY output payload.                                     |

Unknown, malformed, or oversized host-to-client control frames are
preserved as output by `ptyroom join` after the connection is ready. This
keeps child output robust in the face of escape-sequence lookalikes.
Malformed client-to-host controls before `hello;1` disconnect the client;
after `hello;1`, unknown controls are ignored as controls rather than
written to the PTY.

After the hello, client-to-host is a raw terminal input byte stream with
reserved `ptyroom` DCS control frames for terminal geometry.

Host-to-client is a framed stream. Control frames use the reserved
`ptyroom` DCS namespace, but PTY output is sent as length-delimited data
so child output can contain arbitrary escape sequences without being
mistaken for trusted transport control:

```text
ESC P ptyroom;data;<byte-len> ESC \ <byte-len raw PTY output bytes>
```

- Client to host: hello, then raw terminal input bytes except reserved
  `ptyroom` DCS control frames.
- Host to client: hello, then `ptyroom` DCS control frames plus
  length-delimited data frames carrying raw PTY output bytes.
- Recording: PTY output bytes as `.ptytrace` output events, with
  `.ptytrace` resize events whenever the canonical PTY size changes.

When a client joins, `ptyroom host` first sends the current size control
frame, then a bounded replay of recent complete data frames. The replay
buffer evicts whole frames, never arbitrary bytes, so late joiners do not
start in the middle of a length-delimited payload. This keeps late
joiners from seeing a blank terminal until the next output event while
preserving the same framing rules as live output.

This means `ptyroom join` can be simple but not byte-blind: relay stdin to
the socket, parse host-to-client frames, and write only decoded PTY output
bytes to stdout. After piped stdin closes, it half-closes the socket write
side and continues reading output until `ptyroom host` closes.

When both stdin and stdout are terminals, `ptyroom join` also has a
client-local control prefix, `Ctrl-]`. That prefix is not a protocol
frame and is not sent to the host unless the user types `Ctrl-] Ctrl-]`.
Local controls such as `Ctrl-] .` detach only that join client; bare
terminal controls such as `Ctrl-C` and `Esc` remain ordinary remote PTY
input.

## Terminal Geometry

`ptyroom host` owns one canonical PTY size. `ptyroom join` reports its
local terminal size using a reserved client-to-server DCS control frame:

```text
ESC P ptyroom;resize;<cols>;<rows> ESC \
```

`ptyroom host` strips this frame from the input stream, records the
client's latest size, and resizes the child PTY to the smallest known
attached rendering terminal size. The host terminal also participates in
this calculation when local output is enabled and the host stdout is a
terminal. If no rendering terminal has reported a size, `ptyroom host`
uses the configured initial size.

`ptyroom watch` clients send only `hello;1` on the client-to-host
stream. They never emit `resize` frames and never forward stdin bytes,
so a watcher's local terminal size has no effect on the canonical PTY
size and cannot drive the child process.

Whenever a client joins or the canonical size changes, `ptyroom host`
sends a reserved server-to-client DCS control frame:

```text
ESC P ptyroom;size;<cols>;<rows> ESC \
```

Interactive `ptyroom join` clients use that frame to resize a local
`vt100` screen model. They render the model into the local terminal's
alternate screen, clearing unused space so larger terminals see a stable
canvas instead of stale raw-output artifacts. Non-terminal stdout keeps
pipeline behavior: `ptyroom join` strips `ptyroom` control frames and
writes raw PTY output bytes.

Interactive join clients reserve one local status row for controls and
connection state. They report the remaining rows to the host, so the
canonical shared PTY never renders beneath the local-only status line.

This is the first tmux-like geometry rule: no connected rendering
`ptyroom join` client should be smaller than the logical PTY it is
rendering. Larger clients get blank space outside the canonical shared
screen, and removing the smallest client allows the session to grow to
the next smallest active renderer.

## State Machines

Host-side state for each client:

```text
new connection
  -> waiting for hello
  -> ready
  -> input closed or disconnected
```

While waiting for hello, the host buffers only enough bytes to recognize a
split protocol prefix. Raw input, resize controls, unsupported versions,
and oversized unfinished controls disconnect the client. Once ready, raw
bytes are written to the child PTY, resize controls update that client's
reported size, and unknown control frames are ignored.

Join-side host stream state:

```text
connected
  -> waiting for hello
  -> ready
  -> host closed
```

Before the host hello, decoded output or size frames are an error. Once
ready, data frames become local output, size frames resize the local
screen model, and data payloads are interpreted only by byte length. A
zero-length data frame is a no-op and must not stall the following frame.

## Scheduling

`ptyroom host` polls four classes of file descriptor:

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
including any replay bytes queued when it joins. If a client stops
reading and its backlog exceeds the limit, `ptyroom host` disconnects
that client instead of blocking:

- the PTY owner;
- the recorder;
- other clients.

This is the core progress invariant for the current transport: a slow
observer can lose its session, but it cannot stall the shared session.

## Testable Invariants

- Every connection starts with `hello;1`, and unsupported versions fail
  before user bytes reach the PTY.
- PTY output is length-delimited, so child output may contain
  `ESC P ptyroom;... ESC \` byte sequences literally.
- Late replay queues and evicts whole data frames, never partial payloads.
- Interactive joins report `local_rows - 1` so the status line is never
  part of the shared PTY.
- The host canonical size is the minimum known rendering size, falling
  back to the configured initial size when no renderer has reported one.
- A slow client can be disconnected for backlog growth, but cannot block
  the host PTY, recorder, or other clients.
- Local join controls are not protocol frames and are enabled only when
  both stdin and stdout are terminals. `Ctrl-] .` detaches only the local
  join process; bare `Ctrl-C` remains remote input in interactive mode.

## Lifecycle

- The listener is nonblocking and accepts all pending clients whenever it
  becomes readable.
- Client EOF or socket errors remove that client.
- PTY EOF, PTY error, or `--max-secs` ends the session.
- At session end, `ptyroom host` terminates the child process and writes
  the trace.

The CLI summary reports accepted clients, disconnected clients, and
backlog drops. Backlog drops are also counted as disconnects.

## Security Boundary

`ptyroom` has no authentication, authorization, encryption, or replay
protection. A connected client can type into the PTY.

Defaults are conservative:

- `--listen` defaults to loopback.
- Non-loopback binds are refused unless
  `--allow-unauthenticated-public-bind` is present.

For remote use, run `ptyroom` behind SSH, WireGuard, a private overlay
network, or another authenticated transport. In the algebra of the
system, `ptyroom` is only the byte transport; identity is a separate
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
