# Scene DSL — v1 Grammar

A line-oriented domain-specific language for declaring scripted terminal
recordings. Compiles deterministically to an asciinema cast.

The DSL is the binary/file interface to the recorder library. It removes
the need to write Rust to author a recording: scenes are `.scene` files
that `term-recorder record` consumes.

## Design rationale

The DSL is sequential and primitive-oriented. Each verb does exactly
one thing — there is no atomic "send-and-wait" macro at the primitive
level. This is deliberate:

- **Composability.** Sequential primitives form a free monoid — any
  scene is a concatenation of primitives, and concatenation is
  associative. Atomic verbs introduce a denormalization (some
  statements are primitives, some are macros that expand into many).
- **Test surface.** Fewer verb types means fewer code paths to test
  and fewer ways for the recorder library to grow internal complexity.
- **AI-editability.** When an LLM helps a user author a scene,
  inserting a step between two primitives is a one-line edit.
  Splitting an atomic verb to insert is more error-prone.

The contract is **visual equivalence**, not byte-exact event
partitioning. Two scenes that produce the same vt100 screen sequence
at the same playback timestamps are considered equivalent even if
their cast event counts differ. The regression gate's
snapshot-sequence hash is the load-bearing invariant; cast event
count is implementation detail.

## File header

Every scene must begin (after any leading comments or blank lines)
with a version line:

```
Version 1
```

The parser rejects files without a version line and refuses to run
files with a version it does not understand. Future versions are
introduced additively; v1 files keep parsing forever.

## Lexical syntax

| Token           | Form                                                                  | Notes                                                 |
| --------------- | --------------------------------------------------------------------- | ----------------------------------------------------- |
| Comment         | `# rest of line`                                                      | Whole-line or trailing                                |
| String literal  | `"..."` with C escapes (`\n \r \t \\ \" \xNN \e`)                     | Single-line                                           |
| Heredoc literal | `<<NAME` then content; terminator is a line containing exactly `NAME` | Multi-line; bytes verbatim — **no escape processing** |
| Regex           | `/.../ ` — `regex` crate bytes mode                                   | Single-line                                           |
| Integer         | bare digits                                                           | `42`                                                  |
| Duration        | int + unit (`ms`, `s`, `m`)                                           | `500ms`, `2s`, `1m`                                   |
| Key name        | TitleCase identifier                                                  | `Enter Down Up Right Left Tab Esc Space Backspace`    |
| Verb            | TitleCase identifier at start of line                                 | `WaitFor`, `Type`, `SetCols`                          |

A heredoc occupies multiple physical lines but is a single logical
token (the value of one verb argument). Heredocs are valid anywhere a
string literal is valid; single-line strings remain preferred for
short content. The terminator name `NAME` is any TitleCase identifier
the author picks (`EOF`, `BASH`, `END`, etc.) — pick something that
doesn't appear in the content.

Files are UTF-8 only.

## Header verbs (configuration)

Header verbs configure the recording context. They must precede any
body verb; placing one after a body verb is a parse error. Order
within the header is insignificant.

| Verb                                 | Purpose                                      | Default                           |
| ------------------------------------ | -------------------------------------------- | --------------------------------- |
| `SetCols N`                          | Terminal width                               | `80`                              |
| `SetRows N`                          | Terminal height                              | `24`                              |
| `SetSpawn "argv0" "arg1" ...`        | Local process target                         | (one of Spawn/Warm/Cold required) |
| `SetWarm "container_name"`           | Warm container target (`docker exec`)        |                                   |
| `SetCold "image"`                    | Cold container target (`docker run --rm`)    |                                   |
| `SetEnv "KEY" "value"`               | Environment variable; repeatable             | none                              |
| `SetShellRcfile <string-or-heredoc>` | Bash rcfile content (Cold mode only)         | bash default                      |
| `SetMaxRuntime <duration>`           | Wall-time guard against hung child           | `4m`                              |
| `SetPrompt /regex/`                  | Prompt pattern for `Run` and `WaitForPrompt` | `/\$ /`                           |
| `SetPerCharDwell <duration>`         | Default per-character dwell for `Type`       | `35ms`                            |
| `SetPerKeyDwell <duration>`          | Default inter-press dwell for `Press N`      | `35ms`                            |

Constraints:

- Exactly one of `SetSpawn` / `SetWarm` / `SetCold` is required.
- `SetShellRcfile` is meaningful only with `SetCold`. Specifying it
  with Spawn or Warm emits a parse warning; the rcfile is ignored
  because the warm container's shell is already running.

## Body verbs

Body verbs are split into two classes by what they do to the cast:

### Class A — PTY side-effects

These verbs write bytes to the PTY. They do not emit cast events on
their own. Bytes become events when a Class B verb captures them.

| Verb    | Form                                            | Semantics                                                                                                        |
| ------- | ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `Send`  | `Send <string-or-heredoc>`                      | Write raw bytes to PTY.                                                                                          |
| `Press` | `Press <Key> [N] [Dwell <duration>]`            | Send key bytes. Optional repeat count `N`; optional inter-press `Dwell` override (defaults to `SetPerKeyDwell`). |
| `Type`  | `Type <string-or-heredoc> [PerChar <duration>]` | Per-character `Send`, with `PerChar` between characters (defaults to `SetPerCharDwell`).                         |

### Class B — event-producing

| Verb            | Form                                                   | Semantics                                                                                                                                                                                                         |
| --------------- | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `WaitFor`       | `WaitFor /pattern/ [Timeout <duration>] [Label "..."]` | Block until `pattern` matches in PTY output (default timeout `2s`). Captured bytes (up to and including pattern end) become a cast event with `dwell=0`. Trailing bytes return to the drainer for the next event. |
| `WaitForPrompt` | `WaitForPrompt [Timeout <duration>]`                   | Sugar for `WaitFor <SetPrompt regex>`.                                                                                                                                                                            |
| `Sleep`         | `Sleep <duration>`                                     | Extend the most recent event's dwell. No new event. No-op if no events have been emitted yet (the cast naturally starts at `t=0`).                                                                                |
| `Mark`          | `Mark "label"`                                         | Insert a named marker at current presentation time. Trace metadata; not in the cast.                                                                                                                              |
| `Present`       | `Present <string-or-heredoc>`                          | Synthetic output written into the cast as if from the child. One cast event with `Sleep`-extendable dwell.                                                                                                        |

Class A vs Class B is enforced by execution order semantics, not by
the parser. A scene of all Class A verbs produces an empty cast
(no events) — legal but useless.

## Macros

Macros are syntactic sugar; the parser expands them inline before
execution. Their expansions are stable and documented.

| Macro       | Expansion                                |
| ----------- | ---------------------------------------- |
| `Run "cmd"` | `Type "cmd"; Press Enter; WaitForPrompt` |

New macros are additive — they never become primitives.

## Determinism and failure semantics

- **Same scene → same cast bytes**, given the same pipeline identity
  (binary version, ffmpeg version, bundled font hash). Receipts
  capture the identity for external verification.
- **`Sleep` is virtual time** — added to a step's dwell, never
  observed via wall-clock.
- **`WaitFor` failure (timeout)** halts recording with
  `scene.scene:LINE: WaitFor /pattern/ timed out after Nms`. Optional
  `Label` is appended.
- **No source of nondeterminism** in the runner — no random, no
  system-time reads, no host environment unless explicit via
  `SetEnv`.

## Examples

### Minimal (local bash)

```
Version 1
SetSpawn "bash" "--noprofile" "--norc" "-i"
SetEnv "PS1" "$ "
SetEnv "TERM" "xterm-256color"

WaitForPrompt
Run "echo hello"
Run "ls /tmp"
```

### Tint scene (warm container, regex prompt)

```
Version 1
SetCols 100
SetRows 24
SetWarm "term-recorder-warm"
SetPrompt /\[\d+m\] \$ /

WaitForPrompt
Run "tint dracula"
Sleep 1s
Mark "applied_dracula"

Type "tint reset"
Press Enter
WaitForPrompt
Sleep 500ms
```

### Cold container with custom shell profile (heredoc rcfile)

```
Version 1
SetCols 100
SetRows 30
SetCold "debian:12-slim"
SetMaxRuntime 2m
SetEnv "TERM" "xterm-256color"
SetPrompt /\$ /

SetShellRcfile <<BASH
PS1='\[\e[31m\]t\[\e[33m\]i\[\e[32m\]n\[\e[36m\]t\[\e[0m\] $ '
cd "$HOME"
clear
BASH

WaitForPrompt
Sleep 800ms

Present <<EOF
# tint changes terminal color from your CLI

EOF
Sleep 1500ms

Run "tint dracula"
Sleep 1s
Mark "beat_1_dracula"

Run "tint solarized-light"
Sleep 1s
Mark "beat_2_solarized"

Run "tint reset"
Sleep 800ms
```

## Library and CLI integration

```rust
let cast = term_recorder::scene::Scene::read("demo.scene")?.run()?;
cast.write("demo.cast")?;
```

```bash
term-recorder record demo.scene --out demo.cast
term-recorder record demo.scene --out demo.gif         # chains through render
term-recorder record demo.scene \
    --out demo.gif \
    --receipt demo.gif.receipt.json \
    --spec demo.spec.json
```

Receipts gain an optional `scene_sha256` field for full provenance:
the artifact (`output`) is `g(f(scene))` for `f = scene.run` and
`g = render`. A B+C verifier can confirm all three hashes plus run
the spec against the cast.

## Out of scope for v1

These are deliberately deferred. They are not problems we have
today; they would add grammar and complexity without an active use
case.

| Feature                                     | Why deferred                                                     |
| ------------------------------------------- | ---------------------------------------------------------------- |
| `Source "common.scene"` includes            | Scenes are self-contained; revisit if codegen demands it         |
| `Hide` / `Show` (suppress events from cast) | Not load-bearing for current use cases                           |
| Loops, conditionals                         | Use external codegen if you need procedural scenes               |
| `OnTimeout: continue` recovery              | Halt-on-failure is the correct default                           |
| Mid-scene `Set*` verbs                      | Strict header/body separation; revisit only with a real use case |
| Auto-detect cols/rows                       | Would introduce nondeterminism                                   |
| `<<-EOF` indented heredoc variant           | One heredoc form is enough for v1                                |
