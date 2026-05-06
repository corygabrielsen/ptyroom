//! Logical key names → byte sequences.
//!
//! All keys are total: every variant has a non-empty byte sequence and an
//! exhaustive `match` produces a `&'static [u8]`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Key {
    Down,
    Up,
    PickerDown,
    PickerUp,
    Right,
    Left,
    Enter,
    Escape,
    Tab,
    Space,
}

impl Key {
    #[must_use]
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Key::Down => b"\x1b[B",
            Key::Up => b"\x1b[A",
            Key::PickerDown => b"j",
            Key::PickerUp => b"k",
            Key::Right => b"\x1b[C",
            Key::Left => b"\x1b[D",
            Key::Enter => b"\r",
            Key::Escape => b"\x1b",
            Key::Tab => b"\t",
            Key::Space => b" ",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_bytes() {
        for k in [
            Key::Down,
            Key::Up,
            Key::PickerDown,
            Key::PickerUp,
            Key::Right,
            Key::Left,
            Key::Enter,
            Key::Escape,
            Key::Tab,
            Key::Space,
        ] {
            assert!(!k.bytes().is_empty(), "{k:?}");
        }
    }
}
