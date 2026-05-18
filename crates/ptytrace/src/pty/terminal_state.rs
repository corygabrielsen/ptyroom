//! Terminal state cleanup shared by interactive PTY frontends.

use std::io::IsTerminal;
use std::os::fd::{BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering};

#[cfg(not(test))]
use nix::libc;

const NO_SEQUENCE: u8 = 0;
const CHILD_OUTPUT_SEQUENCE: u8 = 1;
const VIEWPORT_SEQUENCE: u8 = 2;
#[cfg(not(test))]
const TERMINATION_SIGNALS: [libc::c_int; 4] =
    [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

// INVARIANT_SIGNAL_RESTORE_FD_IS_STABLE
// ====================================
// `SIGNAL_RESTORE_FD` and `SIGNAL_RESTORE_SEQUENCE` are read from
// async-signal context by `handle_termination_signal`. The handler
// can only do async-signal-safe work — no allocation, no Mutex, no
// most of stdlib. It also has no way to coordinate with a writer:
// if the main thread mutates either atomic between the handler's
// load of FD and its `libc::write` call, the handler writes to a
// stale (or worse, recycled-by-another-thread) fd.
//
// To make this race unreachable, the fd is set EXACTLY ONCE per
// `RestoreGuard` lifetime — in `RestoreGuard::new` — and cleared
// EXACTLY ONCE in `Drop`. There is no `set_fd` API. The session's
// stdout fd is captured once at session start (`share/mod.rs` and
// `connect.rs`) and threaded immutably through all subsequent
// viewport calls, so an in-place fd swap was never needed.
//
// `GUARD_COUNT` enforces the second half of the contract: only one
// `RestoreGuard` may be alive at a time. A second concurrent
// construction would clobber the atomics from under the
// already-installed signal handlers; we panic instead. In practice
// the codebase's guard sites are mutually exclusive (host-viewport
// XOR child-output-tee, see `setup_host_terminal`), so the counter
// is a guard-rail against future regressions, not load-bearing.
//
// Tests for this invariant are platform-specific (signal delivery)
// and intentionally omitted — the invariant is enforced
// structurally by the absence of a mutation API and the runtime
// counter check.
static TERMINATION_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIGNAL_RESTORE_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_RESTORE_SEQUENCE: AtomicU8 = AtomicU8::new(NO_SEQUENCE);
static GUARD_COUNT: AtomicUsize = AtomicUsize::new(0);

// No trailing CRLF: `\x1b[?25h` is the last byte. A `\r\n` tail would
// advance the cursor one row, and at viewport bottom that scrolls
// one row of the user's content into scrollback (or off entirely on
// terminals without scrollback). That's a violation of
// `INVARIANT_USER_SCROLLBACK_PRESERVED` in `ptyrecord/INVARIANTS.md`.
// The cost-vs-benefit is bad: one row of cleaner post-session output
// is not worth one row of destroyed user state. Bleed risk in the
// calling binary's post-session printlns is mitigated by per-row
// `\x1b[2K\r` there (see `ptyrecord::print_wrote`).
const GENERAL_RESTORE_SEQUENCE: &[u8] =
    b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1049l\x1b[?25h";

const VIEWPORT_RESTORE_SEQUENCE: &[u8] =
    b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1049l\x1b[23;2t\x1b[?25h";

/// Enter xterm alternate screen buffer with cursor at home.
///
/// `\x1b[?1049h` switches to the alt-screen but does NOT reset
/// cursor position — the saved-from-primary position carries over.
/// Without the immediate `\x1b[H` (cursor home), the captured
/// session's first prompt would draw at whatever row the user's
/// shell prompt happened to be on (typically mid-screen). tmux,
/// screen, vim, and less all pair the alt-screen enter with a
/// cursor-home for this exact reason.
///
/// Verified in `tests::alt_screen_enter_homes_cursor` — feeds the
/// sequence into a vt100 emulator with the cursor pre-positioned
/// mid-screen and asserts the post-feed cursor is at (0, 0).
const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h\x1b[H";

/// The PTY-frontend enter sequence. Caller should write this to the
/// user's stdout before installing the captured session, after
/// installing a [`RestoreGuard`] keyed on
/// [`child_output_restore_sequence`].
#[must_use]
pub(super) const fn child_output_enter_sequence() -> &'static [u8] {
    ALT_SCREEN_ENTER
}

/// RAII guard that puts a tty fd into raw mode on construction and
/// restores the original termios on drop.
///
/// Both the live-capture path (`pty::live`) and the share path
/// (`pty::share`) need exactly this guard around the host's stdin
/// fd for the duration of an interactive session. Single
/// definition lives here so the two callers can't drift.
///
/// Construction returns the bare `nix` error; the caller decides
/// what context to attach (e.g., "is stdin a tty?" for `live`,
/// "ptyroom host stdin" for `share`).
pub(super) struct RawModeGuard {
    fd: RawFd,
    original: nix::sys::termios::Termios,
}

impl RawModeGuard {
    /// Enter raw mode on `fd`. Returns `Err` if `fd` isn't a tty or
    /// `tcsetattr` fails. The original termios is captured for
    /// restoration on drop.
    ///
    /// # Errors
    /// `tcgetattr` (non-tty fd) or `tcsetattr` (kernel rejected the
    /// raw mode write).
    pub(super) fn enter(fd: RawFd) -> nix::Result<Self> {
        use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let original = tcgetattr(borrowed)?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        tcsetattr(borrowed, SetArg::TCSAFLUSH, &raw)?;
        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        use nix::sys::termios::{SetArg, tcsetattr};
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = tcsetattr(borrowed, SetArg::TCSAFLUSH, &self.original);
    }
}

/// Install a [`RestoreGuard`] for the child-output restore sequence
/// on `fd`, gated on the host's stdout being a tty (and not running
/// under `cfg(test)`, where the restore would inject ANSI noise into
/// captured-output assertions).
///
/// `enabled` is an additional caller-side precondition: pass `true`
/// to always install when tty-eligible, or `false` to skip
/// installation regardless. `share.rs` uses it to skip restoration
/// when the host suppressed local output (no tee → nothing to clean
/// up); `live.rs` always passes `true` because it always tees.
///
/// Returns `None` in three cases: `cfg(test)`, stdout isn't a tty,
/// or `enabled` is `false`. The `None` case is non-fatal — callers
/// just skip the restore.
#[must_use]
pub(super) fn child_output_cleanup_guard(enabled: bool, fd: RawFd) -> Option<RestoreGuard> {
    if cfg!(test) || !enabled {
        return None;
    }
    std::io::stdout()
        .is_terminal()
        .then(|| RestoreGuard::new(fd, child_output_restore_sequence()))
}

/// Cleanup for frontends that pass child PTY output directly to the
/// user's terminal.
#[must_use]
pub(super) const fn child_output_restore_sequence() -> &'static [u8] {
    GENERAL_RESTORE_SEQUENCE
}

/// Cleanup for `ptyroom` viewport mode. Includes a window-title pop so
/// the title set on viewport enter is restored on exit when the
/// terminal supports the xterm title-stack extension.
#[must_use]
pub(super) const fn viewport_restore_sequence() -> &'static [u8] {
    VIEWPORT_RESTORE_SEQUENCE
}

pub(super) struct RestoreGuard {
    fd: RawFd,
    sequence: &'static [u8],
    // `Option` so `Drop` can `.take()` it and force the handler
    // teardown to happen BEFORE clearing the async-signal-safe
    // atomics. The implicit field-drop order would run the
    // restore-fd write and atomic clears between the user-visible
    // Drop body and the handlers' own Drop, which is not the
    // ordering the async-signal-safe contract requires.
    signal_handlers: Option<SignalHandlers>,
}

impl RestoreGuard {
    /// Install signal handlers and arm the async-signal-safe restore
    /// path on `fd`. See `INVARIANT_SIGNAL_RESTORE_FD_IS_STABLE`
    /// above: the fd is set once here and cleared once on drop, and
    /// only one `RestoreGuard` may be alive at a time.
    ///
    /// Install order: signal handlers FIRST, then atomics. A signal
    /// arriving between handler installation and atomic stores would
    /// find a still-clear fd and return harmlessly. The reverse order
    /// would leave atomics armed with no handler to consume them — if
    /// `SignalHandlers::install` returned `None` (sigaction failure),
    /// the atomics would stay non-zero indefinitely and a subsequent
    /// `RestoreGuard::new` would see `GUARD_COUNT` > 0 and the stale
    /// atomic values until the failed guard's Drop runs.
    ///
    /// # Panics
    /// If another `RestoreGuard` is already alive in this process.
    /// A concurrent second guard would overwrite the atomics that
    /// the first guard's installed signal handlers read from,
    /// reintroducing the signal-handler TOCTOU this guard exists to
    /// prevent.
    #[must_use]
    pub(super) fn new(fd: RawFd, sequence: &'static [u8]) -> Self {
        let previous = GUARD_COUNT.fetch_add(1, Ordering::SeqCst);
        // In `cfg(test)`, `SignalHandlers::install` is a no-op stub
        // (see below) so concurrent test threads can each construct a
        // guard against an isolated pipe fd without colliding on real
        // signal-handler installation. The single-guard invariant
        // only matters when real handlers are installed, which is
        // only in `cfg(not(test))`.
        #[cfg(not(test))]
        assert_eq!(
            previous, 0,
            "RestoreGuard: a second guard cannot be installed while one is \
             alive — see INVARIANT_SIGNAL_RESTORE_FD_IS_STABLE",
        );
        #[cfg(test)]
        let _ = previous;
        // Install handlers first; only arm the async-signal-safe
        // atomics after the handler is in place to read them. If
        // install returned `None` (sigaction failure in cfg(not(test))
        // or the cfg(test) stub), skip the atomic stores entirely —
        // there is no handler to consume the armed state, so leaving
        // the atomics at their default (-1 / NO_SEQUENCE) is correct.
        let handlers = SignalHandlers::install();
        if handlers.is_some() {
            clear_termination_request();
            SIGNAL_RESTORE_FD.store(fd, Ordering::SeqCst);
            SIGNAL_RESTORE_SEQUENCE.store(sequence_kind(sequence), Ordering::SeqCst);
        }
        Self {
            fd,
            sequence,
            signal_handlers: handlers,
        }
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        restore_fd_best_effort(self.fd, self.sequence);
        // Uninstall handlers BEFORE clearing atomics. After
        // `signal_handlers.take()` is dropped, the `SignalHandlers`
        // Drop has restored the previous sigactions, so no
        // termination signal can land in our async-signal-safe
        // handler past this point. Only then is it safe to zero the
        // atomics — if we zeroed first, a signal racing between the
        // store and the handler-uninstall would load fd=-1 and
        // return early. Currently safe by accident; this ordering
        // makes "no handler can fire" the explicit precondition for
        // clearing the atomics. `let _ = ...` drops at end of
        // statement (unlike `let _x = ...` which holds the binding
        // until scope end) — verified by the cfg(test) build, which
        // has a unit `SignalHandlers` and is lint-clean here.
        let _ = self.signal_handlers.take();
        SIGNAL_RESTORE_FD.store(-1, Ordering::SeqCst);
        SIGNAL_RESTORE_SEQUENCE.store(NO_SEQUENCE, Ordering::SeqCst);
        clear_termination_request();
        GUARD_COUNT.fetch_sub(1, Ordering::SeqCst);
    }
}

#[must_use]
pub(super) fn termination_requested() -> bool {
    TERMINATION_REQUESTED.load(Ordering::SeqCst)
}

pub(super) fn clear_termination_request() {
    TERMINATION_REQUESTED.store(false, Ordering::SeqCst);
}

pub(super) fn restore_fd_best_effort(fd: RawFd, sequence: &'static [u8]) {
    let _ = super::terminal_io::write_all(fd, sequence);
}

fn sequence_kind(sequence: &'static [u8]) -> u8 {
    if sequence.as_ptr() == child_output_restore_sequence().as_ptr()
        && sequence.len() == child_output_restore_sequence().len()
    {
        CHILD_OUTPUT_SEQUENCE
    } else if sequence.as_ptr() == viewport_restore_sequence().as_ptr()
        && sequence.len() == viewport_restore_sequence().len()
    {
        VIEWPORT_SEQUENCE
    } else {
        CHILD_OUTPUT_SEQUENCE
    }
}

#[cfg(not(test))]
extern "C" fn handle_termination_signal(_signal: libc::c_int) {
    TERMINATION_REQUESTED.store(true, Ordering::SeqCst);
    let fd = SIGNAL_RESTORE_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    match SIGNAL_RESTORE_SEQUENCE.load(Ordering::SeqCst) {
        CHILD_OUTPUT_SEQUENCE => write_signal_safe(fd, child_output_restore_sequence()),
        VIEWPORT_SEQUENCE => write_signal_safe(fd, viewport_restore_sequence()),
        _ => {}
    }
}

#[cfg(not(test))]
fn write_signal_safe(fd: RawFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written <= 0 {
            return;
        }
        let Ok(written) = usize::try_from(written) else {
            return;
        };
        bytes = &bytes[written..];
    }
}

#[cfg(not(test))]
struct SignalHandlers {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

#[cfg(not(test))]
impl SignalHandlers {
    fn install() -> Option<Self> {
        let mut previous = Vec::with_capacity(TERMINATION_SIGNALS.len());
        for signal in TERMINATION_SIGNALS {
            let mut action = empty_sigaction();
            action.sa_sigaction = handle_termination_signal as *const () as libc::sighandler_t;
            action.sa_flags = 0;
            unsafe {
                libc::sigemptyset(&raw mut action.sa_mask);
            }
            let mut old = empty_sigaction();
            let rc = unsafe { libc::sigaction(signal, &raw const action, &raw mut old) };
            if rc != 0 {
                restore_previous_handlers(&previous);
                return None;
            }
            previous.push((signal, old));
        }
        Some(Self { previous })
    }
}

#[cfg(not(test))]
impl Drop for SignalHandlers {
    fn drop(&mut self) {
        restore_previous_handlers(&self.previous);
    }
}

#[cfg(not(test))]
fn restore_previous_handlers(previous: &[(libc::c_int, libc::sigaction)]) {
    for &(signal, action) in previous.iter().rev() {
        unsafe {
            libc::sigaction(signal, &raw const action, std::ptr::null_mut());
        }
    }
}

#[cfg(not(test))]
fn empty_sigaction() -> libc::sigaction {
    unsafe { std::mem::zeroed() }
}

#[cfg(test)]
struct SignalHandlers;

#[cfg(test)]
impl SignalHandlers {
    fn install() -> Option<Self> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use nix::unistd::{pipe, read};

    use super::{
        RestoreGuard, child_output_enter_sequence, child_output_restore_sequence,
        viewport_restore_sequence,
    };

    #[test]
    fn child_output_restore_shows_cursor_after_leaving_alt_screen() {
        assert_cursor_visible_after_alt_screen_exit(child_output_restore_sequence());
    }

    #[test]
    fn viewport_restore_shows_cursor_after_leaving_alt_screen() {
        assert_cursor_visible_after_alt_screen_exit(viewport_restore_sequence());
    }

    #[test]
    fn restore_guard_writes_sequence_on_drop() {
        let (read_fd, write_fd) = pipe().unwrap();

        {
            let _guard = RestoreGuard::new(write_fd.as_raw_fd(), child_output_restore_sequence());
        }
        drop(write_fd);

        let mut output = Vec::new();
        let mut buf = [0_u8; 128];
        loop {
            match read(&read_fd, &mut buf).unwrap() {
                0 => break,
                n => output.extend_from_slice(&buf[..n]),
            }
        }

        assert_eq!(output, child_output_restore_sequence());
    }

    // =====================================================================
    // Visual-effect tests
    //
    // The byte-structure tests above prove "the sequence has the right
    // ANSI codes in the right order." That's necessary but not
    // sufficient — a perfectly-structured sequence can still produce
    // the wrong visual outcome (e.g. alt-screen-enter without cursor-
    // home leaves the captured shell's prompt drawing mid-screen).
    //
    // These tests feed our sequences through a vt100 emulator and
    // assert the OBSERVABLE END STATE. The class of bug they catch:
    // "ANSI escape with valid syntax but wrong effect." When we add a
    // new emitted sequence, add a matching visual-effect test.
    // =====================================================================

    /// Byte-structure assertion: the sequence MUST contain a
    /// cursor-home control code after the alt-screen-enter. Some
    /// terminal emulators (and the vt100 crate we test with) home
    /// the cursor automatically on alt-screen entry; real terminals
    /// like the one our user hit do not. Belt-and-suspenders to
    /// `alt_screen_enter_homes_cursor` below, which can give false
    /// confidence when the test emulator auto-homes.
    #[test]
    fn alt_screen_enter_includes_explicit_cursor_home() {
        let seq = child_output_enter_sequence();
        let enter = b"\x1b[?1049h";
        let enter_pos =
            find_subslice(seq, enter).expect("alt-screen enter sequence missing `\\x1b[?1049h`");
        // After enter, there must be a cursor-home CSI. Accept any of
        // the canonical equivalents.
        let after_enter = &seq[enter_pos + enter.len()..];
        let homes: &[&[u8]] = &[
            b"\x1b[H",    // CSI H — cursor home, parameters default to 1;1
            b"\x1b[1;1H", // CSI 1 ; 1 H — explicit
            b"\x1b[1;1f", // CSI 1 ; 1 f — HVP (equivalent)
        ];
        let has_home = homes
            .iter()
            .any(|h| find_subslice(after_enter, h).is_some());
        assert!(
            has_home,
            "alt-screen enter sequence does not include a cursor-home \
             control code after `\\x1b[?1049h`. Captured shell's first \
             prompt will draw at whatever row/col xterm 1049's saved-
             cursor restoration lands on (typically wherever the user's \
             outer shell prompt was — mid-screen). Without explicit \
             cursor-home, this looks fine in emulators that auto-home \
             on alt-screen entry but breaks on terminals that don't.\n\
             sequence (escaped): {}",
            String::from_utf8_lossy(seq).escape_debug(),
        );
    }

    #[test]
    fn alt_screen_enter_homes_cursor() {
        // Pre-position cursor mid-screen on the primary buffer.
        let mut parser = vt100::Parser::new(40, 80, 0);
        parser.process(b"\x1b[16;31H");
        assert_eq!(
            parser.screen().cursor_position(),
            (15, 30),
            "vt100 setup: cursor not at pre-positioned coords",
        );

        // Feed our alt-screen enter sequence.
        parser.process(child_output_enter_sequence());

        // We must be in alt-screen, cursor must be at home (0, 0).
        // If `\x1b[H` is dropped from the sequence, this fails with
        // cursor at (15, 30) — the position xterm 1049 carried over.
        assert!(
            parser.screen().alternate_screen(),
            "alt-screen enter sequence did not switch to the alt buffer",
        );
        assert_eq!(
            parser.screen().cursor_position(),
            (0, 0),
            "alt-screen enter must home the cursor — captured shell's \
             first prompt would otherwise draw mid-screen",
        );
    }

    #[test]
    fn child_output_restore_returns_to_primary_with_saved_cursor() {
        // Set up a realistic flow: cursor at (8, 0) on primary
        // (where the user's binary banner ended), enter alt-screen,
        // captured shell scribbles all over the alt buffer, then
        // we emit the restore sequence.
        let mut parser = vt100::Parser::new(40, 80, 0);
        parser.process(b"[recording \xe2\x86\x92 /tmp/x.ptytrace]\r\n");
        let pre_alt = parser.screen().cursor_position();
        // Enter alt-screen (mimic capture path).
        parser.process(child_output_enter_sequence());
        // Captured shell moves cursor, draws prompt, runs command.
        parser.process(b"\x1b[20;40H~ $ ls\r\nfoo\nbar\nbaz\n");

        // Restore.
        parser.process(child_output_restore_sequence());

        // We should be back on primary, cursor at the saved
        // position (right after the banner). Without the
        // restore sequence's `\x1b[?1049l` this stays on alt-
        // screen; if the saved-cursor restoration is broken,
        // the cursor lands somewhere else.
        assert!(
            !parser.screen().alternate_screen(),
            "restore sequence did not switch back to the primary buffer",
        );
        assert_eq!(
            parser.screen().cursor_position(),
            pre_alt,
            "restore sequence did not return cursor to pre-alt-screen position",
        );
    }

    #[test]
    fn viewport_restore_returns_to_primary_with_saved_cursor() {
        let mut parser = vt100::Parser::new(40, 80, 0);
        parser.process(b"[ptyroom host]\r\n");
        let pre_alt = parser.screen().cursor_position();
        parser.process(child_output_enter_sequence());
        parser.process(b"\x1b[10;20Hsome host-viewport content");

        parser.process(viewport_restore_sequence());

        assert!(!parser.screen().alternate_screen());
        assert_eq!(parser.screen().cursor_position(), pre_alt);
    }

    /// Round-trip: enter alt-screen, captured-session stuff, exit.
    /// User's primary screen should be bit-identical to its pre-
    /// session state. This is the end-to-end UX promise.
    #[test]
    fn alt_screen_round_trip_preserves_primary_content() {
        let mut parser = vt100::Parser::new(40, 80, 0);
        // Pre-session: simulate a few rows of user shell history.
        parser.process(
            b"$ ls -la\r\n\
              file1  file2  file3\r\n\
              $ cargo run --bin ptyrecord zsh\r\n\
              [recording \xe2\x86\x92 /tmp/x.ptytrace]\r\n",
        );
        // Snapshot primary content + cursor BEFORE alt-screen.
        let cursor_before = parser.screen().cursor_position();
        let row0_before = parser.screen().rows(0, 80).next().unwrap();
        let row1_before = parser.screen().rows(0, 80).nth(1).unwrap();

        // Enter alt-screen, do stuff, exit.
        parser.process(child_output_enter_sequence());
        parser.process(b"\x1b[5;5Hcaptured session output everywhere\r\n");
        parser.process(b"\x1b[20;1Hmore stuff\r\n");
        parser.process(child_output_restore_sequence());

        // After exit: primary content + cursor must match before.
        assert!(!parser.screen().alternate_screen());
        assert_eq!(
            parser.screen().cursor_position(),
            cursor_before,
            "cursor not restored to pre-session position",
        );
        let row0_after = parser.screen().rows(0, 80).next().unwrap();
        let row1_after = parser.screen().rows(0, 80).nth(1).unwrap();
        assert_eq!(row0_before, row0_after, "primary row 0 mutated");
        assert_eq!(row1_before, row1_after, "primary row 1 mutated");
    }

    fn assert_cursor_visible_after_alt_screen_exit(sequence: &[u8]) {
        let alt_screen_exit = b"\x1b[?1049l";
        let show_cursor = b"\x1b[?25h";
        let alt_pos = find_subslice(sequence, alt_screen_exit).unwrap();
        let final_show_pos = sequence
            .windows(show_cursor.len())
            .rposition(|window| window == show_cursor)
            .unwrap();

        // Show-cursor must come after alt-screen-exit and must be the
        // last byte of the sequence, so the cursor ends up visible
        // and the restore sequence does not advance the cursor (which
        // would risk scrolling user state — see
        // `INVARIANT_USER_SCROLLBACK_PRESERVED` in
        // `ptyrecord/INVARIANTS.md`).
        assert!(alt_pos < final_show_pos);
        assert!(sequence.ends_with(show_cursor));
    }

    use super::super::room_protocol::find_subslice;
}
