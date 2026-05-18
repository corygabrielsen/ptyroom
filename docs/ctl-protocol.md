# `ptyroom ctl` Command Convention

`ptyroom ctl` is the operator-side admin interface for talking to a
running `ptyroom host` process. It is distinct from the user-facing
verbs (`host`, `join`, `watch`): those drive a room from the outside;
`ctl` reaches into a host that is already running.

## What `ctl` is

- **Local admin channel.** `ctl` connects to a per-host Unix-domain
  control socket, not to the room's TCP port. The control socket lives
  under the host's resolved state directory (`PTYROOM_STATE_DIR`,
  `$XDG_RUNTIME_DIR/ptyroom/`, or `/tmp/ptyroom-<euid>/`) as
  `<port>.sock`.
- **Localhost only by design.** The transport is a Unix socket on the
  same machine as the host. There is no network admin surface; remote
  admin must go over SSH (or another trusted tunnel) into a shell on
  the host machine.
- **Targets one host.** The `<addr>` argument is the same `host:port`
  printed by `ptyroom host`; `ctl` uses the port to find the socket,
  not to open a TCP connection.

## Command shape

```text
ptyroom ctl <addr> <namespace> <action> [args]
```

- `<addr>` — the room's `host:port` (used to locate the host's local
  control socket).
- `<namespace>` — a **noun** grouping related operator actions
  (`queue`, and future additions like `clients`, `session`, ...).
- `<action>` — a **verb** scoped to that namespace
  (`add`, `next`, `list`, `clear`, ...).
- `[args]` — action-specific arguments.

This is a two-level subcommand tree: `ctl` is the operator entrypoint,
the namespace narrows to a subsystem, and the action picks the
operation within that subsystem.

## Why nested (noun → verb)

The top-level `ptyroom` verb space (`host`, `join`, `watch`, `ctl`)
describes **what the user is doing with a room**. Operator commands do
not belong in that space:

- Top-level verbs stay short and user-meaningful.
- Operator commands cluster under nouns that describe **what is being
  managed** (a queue, a set of clients, the session), not a flat list
  of verbs that pollute the user-facing namespace.
- New admin surfaces extend by adding a new noun, not by negotiating
  for a top-level verb slot or a unique flat-verb name.

## Existing namespace: `queue`

The host maintains a message queue. Actions:

| Action  | Purpose                                                                     |
| ------- | --------------------------------------------------------------------------- |
| `add`   | Append a message. Argument text inline, or read from stdin until EOF.       |
| `next`  | Inject the next queued message into the shared PTY, followed by `Enter`.    |
| `list`  | Print the current queue depth.                                              |
| `clear` | Drop all queued messages without injecting them.                            |

Example:

```text
ptyroom ctl 127.0.0.1:54321 queue add "hello room"
ptyroom ctl 127.0.0.1:54321 queue list
ptyroom ctl 127.0.0.1:54321 queue next
ptyroom ctl 127.0.0.1:54321 queue clear
```

## Adding a new namespace

Pick a noun for the subsystem, then verbs for the operations. The noun
is the new clap subcommand under `CtlNamespace`; the verbs are a
nested subcommand enum on that variant.

Hypothetical examples (not implemented):

- `ctl <addr> clients list` — enumerate connected clients.
- `ctl <addr> clients kick <client-id>` — disconnect one client.
- `ctl <addr> clients count` — print the active client count.
- `ctl <addr> session info` — report session metadata.
- `ctl <addr> session kill` — terminate the host process cleanly.

The pattern is consistent: **noun first, then verb**. A reader who
knows the convention can predict the command shape without consulting
help text.

## Anti-pattern: flat verbs

Do **not** flatten operator commands into the top level:

- `ctl <addr> kick <client-id>` — pollutes the verb space; forces every
  future command to either join the flat list or introduce nesting
  inconsistently.
- `ctl <addr> list-clients` — kebab-cases a noun-verb pair into a
  pseudo-verb; defeats the grouping the namespace gives for free.
- `ctl <addr> queue-add` / `ctl <addr> queue-list` — same failure mode;
  the noun is present but the structure is not.

If a new admin operation does not fit an existing noun, add a new
namespace. Do not flatten.

## Wire format

The on-the-wire protocol is a single-line verb (optionally followed by
a length-prefixed payload) over the Unix-domain control socket. The
CLI shape above is a thin client over that protocol; the namespace and
action together map to a single wire verb.

Wire format reference and parser:
[`crates/ptytrace/src/pty/share/ctl.rs`](../crates/ptytrace/src/pty/share/ctl.rs).

This document is the authority on the **CLI convention**; the source
above is the authority on the **wire encoding**. Do not duplicate the
wire format here.
