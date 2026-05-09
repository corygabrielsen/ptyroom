//! Join-local input control state.

pub(super) const LOCAL_ESCAPE: u8 = 0x1d; // Ctrl-]
pub(super) const LOCAL_ESCAPE_NAME: &str = "^]";

#[derive(Debug, Default)]
pub(super) struct LocalInputRouter {
    mode: LocalInputMode,
}

impl LocalInputRouter {
    pub(super) fn push(&mut self, byte: u8) -> LocalInputAction {
        match self.mode {
            LocalInputMode::Forward => self.push_forward(byte),
            LocalInputMode::Command => self.push_command(byte),
        }
    }

    fn push_forward(&mut self, byte: u8) -> LocalInputAction {
        if byte == LOCAL_ESCAPE {
            self.mode = LocalInputMode::Command;
            LocalInputAction::SetStatus(LocalStatus::Command)
        } else {
            LocalInputAction::Remote(byte)
        }
    }

    fn push_command(&mut self, byte: u8) -> LocalInputAction {
        self.mode = LocalInputMode::Forward;
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
pub(super) enum LocalInputAction {
    Remote(u8),
    SetStatus(LocalStatus),
    ForceRedraw,
    Disconnect,
    UnknownCommand(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LocalStatus {
    Connected,
    Command,
    Help,
}

#[cfg(test)]
mod tests {
    use super::{LOCAL_ESCAPE, LocalInputAction, LocalInputRouter, LocalStatus};

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
}
