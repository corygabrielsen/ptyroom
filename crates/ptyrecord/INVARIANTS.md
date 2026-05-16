# ptyrecord invariants

These are the contracts ptyrecord (and by extension `ptyroom host` +
`ptytrace capture`) make with the user. Each invariant has:

- A **name** in `SCREAMING_SNAKE_CASE`, referenced by that name in source
  comments and tests so a future reader can trace why a given line of
  code exists.
- A short **statement** of what we promise.
- A **rationale** for why this invariant is at the priority it is.
- A pointer to the **test** that verifies it.

When two invariants conflict, the **priority order** below is binding:
hard invariants beat soft ones, and tradeoffs must be argued in
writing (commit message + this file) before being landed.

## Hard invariants (must hold)

### `INVARIANT_CAPTURED_SESSION_IN_ALT_SCREEN`

ptyrecord wraps the entire captured PTY session in xterm's alternate
screen buffer (`\x1b[?1049h\x1b[H` … `\x1b[?1049l`). The captured
session's output — prompts, command output, scrolling, vt100 escapes
— all happens on the alternate buffer, leaving the user's primary
screen and its scrollback untouched. On exit, the alternate buffer
is discarded and the primary screen is restored exactly as it was,
with the cursor at the position where alt-screen was entered (a
fresh row right below the calling binary's banner).

The `\x1b[H` (cursor home) immediately after `\x1b[?1049h` is
load-bearing. xterm's 1049 saves the primary cursor position and
switches to alt-screen but does NOT reset cursor position — the
captured shell's first prompt would otherwise draw at whatever row
the user's prompt happened to be on. tmux/screen/vim/less all pair
the enter with a home for the same reason.

**Rationale.** This is the same pattern tmux, screen, vim, less, and
fzf use. It is the canonical answer to "I want to take over the
user's terminal for a while without destroying their pre-session
state." Without it, ptyrecord forwards the captured session's bytes
to the primary screen — every command's output piles up, the user's
shell history scrolls off, and post-session printlns land on top of
whatever state the captured session left behind. With it, the
captured session is a self-contained transient view that vanishes on
exit. The user's terminal state at exit is bit-identical to its
state at ptyrecord launch, modulo whatever ptyrecord itself printed
to stderr (a banner before, a `wrote PATH` line after).

This invariant is what makes `INVARIANT_NOTIFICATION_BEST_EFFORT`
nearly always clean in practice: when alt-screen-exit restores the
cursor to a known-fresh row, the `wrote` println has no stale row
content to bleed past.

**Verified by:** `tests/invariants.rs::captured_session_in_alt_screen`.

### `INVARIANT_CONTRACT_FILES_EXIST`

After ptyrecord exits with status 0, every path it announces on stdout
exists on disk and contains the artifact named by its extension
(`.ptyrecord`, `.mp4`, `.ptytrace`).

**Rationale.** The files are the product. Without this, ptyrecord is
useless and untrustworthy. This is the *primary* contract; everything
else exists to support it.

**Verified by:** `tests/invariants.rs::contract_files_exist`.

### `INVARIANT_USER_SCROLLBACK_PRESERVED`

ptyrecord does not emit terminal control sequences that scroll prior
visible content out of the user's viewport on its own initiative.
Newlines from `println!` are exempt because they are inherent to
producing output — but ptyrecord must not emit *padding* newlines for
the purpose of pushing content away.

**Rationale.** Commit `208ad80` violated this by emitting `2 × rows`
newlines to "scroll prior content into scrollback for cleaner output."
On terminals where scrollback was not preserved (or was hard to
recover), this destroyed the user's view of their pre-session work.
A user reported this with: "now my whole history is deleted wtf!!!!!"
Visual cleanliness of our own status output is never worth destroying
the user's state. Always.

The captured session's own behavior (alt-screen, internal scrolling)
is governed by the captured program — ptyrecord forwards those bytes
faithfully and is not responsible for them.

**Verified by:** `tests/invariants.rs::scrollback_preserved`.

### `INVARIANT_USER_TERMINAL_NOT_CLEARED`

ptyrecord does not emit screen-clearing control sequences
(`\x1b[2J`, `\x1bc`, `\x1b[3J`, etc.) on its own initiative.

Per-row clear (`\x1b[2K\r`) immediately preceding a `println!` we
are about to do is **permitted** — it only affects the row we are
about to overwrite, which is a row whose previous content we'd
overwrite anyway with the println. It does not touch other rows or
scrollback.

**Rationale.** Same as `INVARIANT_USER_SCROLLBACK_PRESERVED`: don't
take destructive action on terminal state we don't own.

**Verified by:** `tests/invariants.rs::no_screen_clear_sequences`.

### `INVARIANT_PIPED_STDOUT_IS_PLAIN`

When stdout is not a tty (piped, redirected to a file, captured by a
test), ptyrecord emits zero ANSI escape sequences in its stdout. Lines
are `wrote PATH\n` with no styling, no carriage-return-rewrites, no
clear codes.

**Rationale.** Scripted consumers need to parse our output. Any escape
bytes pollute the data. The defensive escapes we DO emit (per-row
clear) are gated on `IsTerminal::is_terminal()` so they vanish when
stdout isn't a tty.

**Verified by:** `tests/invariants.rs::piped_stdout_is_plain`.

## Soft invariants (best effort)

### `INVARIANT_NOTIFICATION_BEST_EFFORT`

ptyrecord prints one `wrote PATH` line per persistent artifact to
stdout. The intent is the user sees their newly-written files.

**Why this is soft.** The terminal state at exit is unknown — alt-
screen residue, mid-row cursors, content on rows below, mid-session
resize — and we cannot reliably write to a terminal whose state we
don't own. The hard invariants above forbid the aggressive
manipulation that would be required to guarantee zero visual
artifacts. So in extreme terminal states the user may see minor
artifacts (e.g. a tail of a previous row peeking past our text).
The files on disk (`INVARIANT_CONTRACT_FILES_EXIST`) are the
authoritative source — the user can `ls` to verify.

**Defensive measures we DO take** (all non-destructive):

- `\x1b[2K\r` before each `wrote` line when stdout is a tty:
  clears the row we're about to write so partial overwrites don't
  leave tail bleed.
- `\r\n` appended to `GENERAL_RESTORE_SEQUENCE` (see
  `ptytrace::pty::terminal_state`): advances cursor one row past
  whatever the alt-screen exit cursor-restore landed on, so the
  first post-session println lands on a fresh row in the common
  case.

**Defensive measures we DO NOT take** (would violate hard invariants):

- Scrolling padding newlines to push prior content into scrollback.
- Screen clears (`\x1b[2J`, `\x1bc`).
- Forced alt-screen toggles.
- Cursor-absolute positioning to a guessed-known "safe" location.

**Verified by:** `tests/invariants.rs::notification_best_effort_emits_lines`.

## How to evolve this file

If a new bug class emerges:

1. Decide whether it requires adding a new invariant or relaxing an
   existing one.
2. Update this file *first*. Commit the doc change explaining the
   tradeoff before code changes that implement it.
3. Update or add the corresponding test in `tests/invariants.rs`.
4. Reference the named invariant from any code comment that exists
   because of it.

Never relax a hard invariant silently. Never patch a symptom while
violating the hard invariants. If the math doesn't work, accept the
soft-invariant artifact and move on.
