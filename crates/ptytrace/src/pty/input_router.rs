//! Local-input prefix routing for `ptyroom` viewports.
//!
//! A small state machine that intercepts a reserved prefix byte
//! ([`LOCAL_ESCAPE`], `Ctrl-]`) and turns following bytes into local
//! actions (detach/end, redraw, help, doubled-escape) instead of
//! forwarding them to the shared PTY. Both `ptyroom join` / `watch` and
//! `ptyroom host` use the same router; callers map [`LocalInputAction`]
//! to mode-specific behavior (e.g. a join's `Disconnect` closes the TCP
//! socket; a host's `Disconnect` ends the session).
//!
//! ## Command-mode idle timeout
//!
//! `Ctrl-]` followed by no follow-up byte previously left the router
//! locked in Command mode until the next keystroke — minutes or hours
//! later — at which point the keystroke was reinterpreted as a command.
//! Callers now drive [`LocalInputRouter::tick`] each poll iteration
//! with `Instant::now()`; the router auto-returns to Forward after
//! [`COMMAND_MODE_TIMEOUT`] of idleness and yields a `SetStatus`
//! action so the status bar reflects the reset.

use std::time::{Duration, Instant};

pub(crate) const LOCAL_ESCAPE: u8 = 0x1d; // Ctrl-]
pub(crate) const LOCAL_ESCAPE_NAME: &str = "^]";

/// Maximum time the router stays in Command mode without a follow-up
/// byte before reverting to Forward.
pub(crate) const COMMAND_MODE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub(crate) struct LocalInputRouter {
    mode: LocalInputMode,
    /// Instant at which Command mode was entered. `None` in Forward
    /// mode and immediately after a Command-mode transition produced
    /// a terminal action.
    command_entered_at: Option<Instant>,
}

impl LocalInputRouter {
    pub(crate) fn push(&mut self, byte: u8) -> LocalInputAction {
        self.push_at(byte, Instant::now())
    }

    /// Like [`Self::push`], but accepts an explicit `now` so tests can
    /// drive the idle-timeout deterministically.
    pub(crate) fn push_at(&mut self, byte: u8, now: Instant) -> LocalInputAction {
        match self.mode {
            LocalInputMode::Forward => self.push_forward(byte, now),
            LocalInputMode::Command => self.push_command(byte),
        }
    }

    /// Drive the idle-timeout clock. Call this once per poll iteration
    /// with the current `Instant`. If Command mode has been idle for
    /// longer than [`COMMAND_MODE_TIMEOUT`], returns
    /// `Some(SetStatus(Connected))` to acknowledge the reset; otherwise
    /// returns `None` and leaves state untouched.
    pub(crate) fn tick(&mut self, now: Instant) -> Option<LocalInputAction> {
        if self.mode != LocalInputMode::Command {
            return None;
        }
        let entered = self.command_entered_at?;
        if now.saturating_duration_since(entered) < COMMAND_MODE_TIMEOUT {
            return None;
        }
        self.mode = LocalInputMode::Forward;
        self.command_entered_at = None;
        Some(LocalInputAction::SetStatus(LocalStatus::Connected))
    }

    fn push_forward(&mut self, byte: u8, now: Instant) -> LocalInputAction {
        if byte == LOCAL_ESCAPE {
            self.mode = LocalInputMode::Command;
            self.command_entered_at = Some(now);
            LocalInputAction::SetStatus(LocalStatus::Command)
        } else {
            LocalInputAction::Remote(byte)
        }
    }

    fn push_command(&mut self, byte: u8) -> LocalInputAction {
        self.mode = LocalInputMode::Forward;
        self.command_entered_at = None;
        match byte {
            b'.' => LocalInputAction::Disconnect,
            b'?' => LocalInputAction::SetStatus(LocalStatus::Help),
            b'r' | b'R' => LocalInputAction::ForceRedraw,
            LOCAL_ESCAPE => LocalInputAction::Remote(LOCAL_ESCAPE),
            _ => LocalInputAction::UnknownCommand(byte),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum LocalInputMode {
    #[default]
    Forward,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalInputAction {
    Remote(u8),
    SetStatus(LocalStatus),
    ForceRedraw,
    Disconnect,
    UnknownCommand(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalStatus {
    Connected,
    Command,
    Help,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        COMMAND_MODE_TIMEOUT, LOCAL_ESCAPE, LocalInputAction, LocalInputRouter, LocalStatus,
    };

    #[test]
    fn regular_control_bytes_are_remote_input() {
        let mut router = LocalInputRouter::default();

        assert_eq!(router.push(0x03), LocalInputAction::Remote(0x03));
        assert_eq!(router.push(0x1b), LocalInputAction::Remote(0x1b));
    }

    #[test]
    fn prefix_enters_local_command_mode() {
        let mut router = LocalInputRouter::default();

        assert_eq!(
            router.push(LOCAL_ESCAPE),
            LocalInputAction::SetStatus(LocalStatus::Command)
        );
        assert_eq!(
            router.push(b'?'),
            LocalInputAction::SetStatus(LocalStatus::Help)
        );
    }

    #[test]
    fn local_command_mode_maps_commands() {
        let mut router = LocalInputRouter::default();

        assert_eq!(
            router.push(LOCAL_ESCAPE),
            LocalInputAction::SetStatus(LocalStatus::Command)
        );
        assert_eq!(router.push(b'r'), LocalInputAction::ForceRedraw);
        assert_eq!(
            router.push(LOCAL_ESCAPE),
            LocalInputAction::SetStatus(LocalStatus::Command)
        );
        assert_eq!(
            router.push(LOCAL_ESCAPE),
            LocalInputAction::Remote(LOCAL_ESCAPE)
        );
        assert_eq!(
            router.push(LOCAL_ESCAPE),
            LocalInputAction::SetStatus(LocalStatus::Command)
        );
        assert_eq!(router.push(b'.'), LocalInputAction::Disconnect);
    }

    #[test]
    fn unknown_local_command_exits_command_mode() {
        let mut router = LocalInputRouter::default();

        assert_eq!(
            router.push(LOCAL_ESCAPE),
            LocalInputAction::SetStatus(LocalStatus::Command)
        );
        assert_eq!(router.push(b'x'), LocalInputAction::UnknownCommand(b'x'));
        assert_eq!(router.push(b'y'), LocalInputAction::Remote(b'y'));
    }

    #[test]
    fn tick_before_timeout_keeps_command_mode() {
        let mut router = LocalInputRouter::default();
        let t0 = Instant::now();
        assert_eq!(
            router.push_at(LOCAL_ESCAPE, t0),
            LocalInputAction::SetStatus(LocalStatus::Command)
        );
        // Still inside the idle window.
        assert!(router.tick(t0 + COMMAND_MODE_TIMEOUT / 2).is_none());
        // Follow-up byte is still treated as a command.
        assert_eq!(
            router.push_at(b'r', t0 + COMMAND_MODE_TIMEOUT / 2),
            LocalInputAction::ForceRedraw
        );
    }

    #[test]
    fn tick_after_timeout_exits_command_mode() {
        let mut router = LocalInputRouter::default();
        let t0 = Instant::now();
        assert_eq!(
            router.push_at(LOCAL_ESCAPE, t0),
            LocalInputAction::SetStatus(LocalStatus::Command)
        );
        // Past the timeout: tick must reset and emit a Connected status.
        let action = router.tick(t0 + COMMAND_MODE_TIMEOUT + Duration::from_millis(1));
        assert_eq!(
            action,
            Some(LocalInputAction::SetStatus(LocalStatus::Connected))
        );
        // Idempotent: a second tick after the reset is a no-op.
        assert!(
            router
                .tick(t0 + COMMAND_MODE_TIMEOUT + Duration::from_mins(1))
                .is_none()
        );
        // The next byte routes as Forward, not Command.
        assert_eq!(
            router.push_at(b'r', t0 + Duration::from_mins(1)),
            LocalInputAction::Remote(b'r')
        );
    }

    #[test]
    fn tick_in_forward_mode_is_noop() {
        let mut router = LocalInputRouter::default();
        assert!(router.tick(Instant::now()).is_none());
    }
}
