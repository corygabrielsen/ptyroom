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

There is no framing layer yet.

- Client to host: raw terminal input bytes.
- Host to client: raw PTY output bytes.
- Recording: PTY output bytes only, written as `.ptytrace` output events.

This means `ptyconnect` can be simple: relay stdin to the socket and the
socket to stdout. After piped stdin closes, it half-closes the socket
write side and continues reading output until `ptyshare` closes.

## Scheduling

`ptyshare` polls four classes of file descriptor:

- listener socket for new clients;
- host stdin, when local input is enabled;
- the PTY master;
- connected client sockets.

Input ordering is the order in which the host event loop drains readable
participants. There is no causal metadata or per-client identity in the
trace yet. If identity matters, put an authenticated layer in front of
the TCP stream and attach that identity through a future attestation.

## Backpressure

Client output is nonblocking. Each client has a bounded output backlog.
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
- Terminal resize propagation.
- File transfer or side channels.

Those are compatible future layers, but they are not part of the current
raw stream contract.
