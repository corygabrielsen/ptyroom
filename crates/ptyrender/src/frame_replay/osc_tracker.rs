//! OSC state tracker for snapshot replay.
//!
//! Sniffs trace bytes for the OSC sequences that affect terminal default
//! colors and palette overrides, maintaining the running state needed
//! to populate each per-frame [`Frame`]'s `bg`, `fg`, and `palette`
//! fields. Independent of any particular terminal-emulator backend
//! (e.g. `vt100`), because those backends typically expose only SGR
//! per-cell colors, not the terminal-default state we need here.
//!
//! Sequences observed:
//! - `OSC 11 ; <color> ST` — set terminal background.
//! - `OSC 10 ; <color> ST` — set terminal foreground.
//! - `OSC 4 ; idx ; <color> [; idx ; <color>...] ST` — set palette entries.
//! - `OSC 111 ST` — reset terminal background to default.
//! - `OSC 110 ST` — reset terminal foreground to default.
//! - `OSC 104 [; idx ; idx...] ST` — reset palette (all entries when no
//!   indices supplied).
//!
//! `<color>` is `rgb:RR[RR]/GG[GG]/BB[BB]` (xterm-spec) or `#RRGGBB`
//! (compact form). Both terminators (`\x1b\\` ST and `\x07` BEL) accepted.
//!
//! [`Frame`]: crate::frame::Frame

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::color::HexColor;
use ptytrace::pty::StubColors;

/// Running OSC state observable while replaying a trace.
#[derive(Debug, Clone)]
pub struct OscTracker {
    bg: HexColor,
    fg: HexColor,
    palette: BTreeMap<u8, HexColor>,
    defaults: StubColors,
}

impl OscTracker {
    /// New tracker initialised to `defaults` (typically
    /// [`StubColors::default()`] to mirror the recorder's stub).
    #[must_use]
    pub fn new(defaults: StubColors) -> Self {
        Self {
            bg: defaults.bg,
            fg: defaults.fg,
            palette: BTreeMap::new(),
            defaults,
        }
    }

    /// Current terminal background color.
    #[must_use]
    pub fn bg(&self) -> HexColor {
        self.bg
    }

    /// Current terminal foreground color.
    #[must_use]
    pub fn fg(&self) -> HexColor {
        self.fg
    }

    /// Current palette overrides (sparse; entries not in the map use the
    /// emulator's intrinsic 256-color palette).
    #[must_use]
    pub fn palette(&self) -> &BTreeMap<u8, HexColor> {
        &self.palette
    }

    /// Apply every OSC sequence found in `chunk`, updating tracker state.
    /// Bytes that aren't OSC sequences are ignored.
    pub fn observe(&mut self, chunk: &[u8]) {
        for op in scan(chunk) {
            self.apply(op);
        }
    }

    fn apply(&mut self, op: OscOp) {
        match op {
            OscOp::SetBg(c) => self.bg = c,
            OscOp::SetFg(c) => self.fg = c,
            OscOp::SetPalette(entries) => {
                for (idx, color) in entries {
                    self.palette.insert(idx, color);
                }
            }
            OscOp::ResetBg => self.bg = self.defaults.bg,
            OscOp::ResetFg => self.fg = self.defaults.fg,
            OscOp::ResetPaletteAll => self.palette.clear(),
            OscOp::ResetPaletteIdxs(indices) => {
                for idx in indices {
                    self.palette.remove(&idx);
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OscOp {
    SetBg(HexColor),
    SetFg(HexColor),
    SetPalette(Vec<(u8, HexColor)>),
    ResetBg,
    ResetFg,
    ResetPaletteAll,
    ResetPaletteIdxs(Vec<u8>),
}

/// Regex for any OSC sequence we care about. Matches the leading
/// `OSC` byte sequence + payload + terminator. We then parse the
/// payload imperatively because OSC 4 can carry an arbitrary number
/// of `idx;color` pairs.
fn osc_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // OSC = ESC ] ... (ST | BEL); ST = ESC \\.
        Regex::new(r"\x1b\](?P<body>[^\x07\x1b]*)(?:\x1b\\|\x07)").unwrap()
    })
}

fn parse_color(s: &str) -> Option<HexColor> {
    HexColor::parse(s)
}

fn scan(chunk: &[u8]) -> Vec<OscOp> {
    let mut out = Vec::new();
    for cap in osc_re().captures_iter(chunk) {
        let Some(body) = cap.name("body") else {
            continue;
        };
        let Ok(body) = std::str::from_utf8(body.as_bytes()) else {
            continue;
        };
        let mut parts = body.split(';');
        let Some(code) = parts.next() else {
            continue;
        };
        match code {
            "11" => {
                if let Some(c) = parts.next().and_then(parse_color) {
                    out.push(OscOp::SetBg(c));
                }
            }
            "10" => {
                if let Some(c) = parts.next().and_then(parse_color) {
                    out.push(OscOp::SetFg(c));
                }
            }
            "4" => {
                let mut entries = Vec::new();
                let rest: Vec<&str> = parts.collect();
                let mut i = 0;
                while i + 1 < rest.len() {
                    if let (Ok(idx), Some(color)) =
                        (rest[i].parse::<u8>(), parse_color(rest[i + 1]))
                    {
                        entries.push((idx, color));
                    }
                    i += 2;
                }
                if !entries.is_empty() {
                    out.push(OscOp::SetPalette(entries));
                }
            }
            "111" => out.push(OscOp::ResetBg),
            "110" => out.push(OscOp::ResetFg),
            "104" => {
                let indices: Vec<u8> = parts.filter_map(|s| s.parse().ok()).collect();
                if indices.is_empty() {
                    out.push(OscOp::ResetPaletteAll);
                } else {
                    out.push(OscOp::ResetPaletteIdxs(indices));
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> HexColor {
        HexColor::from_rgb(r, g, b)
    }

    fn fresh() -> OscTracker {
        OscTracker::new(StubColors::default())
    }

    #[test]
    fn defaults_match_stub_colors() {
        let t = fresh();
        assert_eq!(t.bg(), StubColors::default().bg);
        assert_eq!(t.fg(), StubColors::default().fg);
        assert!(t.palette().is_empty());
    }

    #[test]
    fn osc_11_set_bg_hash_form() {
        let mut t = fresh();
        t.observe(b"\x1b]11;#282a36\x1b\\");
        assert_eq!(t.bg(), rgb(0x28, 0x2a, 0x36));
    }

    #[test]
    fn osc_11_set_bg_rgb_form_bel_terminated() {
        let mut t = fresh();
        t.observe(b"\x1b]11;rgb:00/ff/00\x07");
        assert_eq!(t.bg(), rgb(0, 0xff, 0));
    }

    #[test]
    fn osc_10_set_fg() {
        let mut t = fresh();
        t.observe(b"\x1b]10;#aabbcc\x1b\\");
        assert_eq!(t.fg(), rgb(0xaa, 0xbb, 0xcc));
    }

    #[test]
    fn osc_4_palette_single_pair() {
        let mut t = fresh();
        t.observe(b"\x1b]4;5;#112233\x1b\\");
        assert_eq!(t.palette().get(&5u8), Some(&rgb(0x11, 0x22, 0x33)));
    }

    #[test]
    fn osc_4_palette_multi_pair() {
        let mut t = fresh();
        t.observe(b"\x1b]4;0;#000000;1;#ffffff;2;#808080\x1b\\");
        assert_eq!(t.palette().get(&0u8), Some(&rgb(0, 0, 0)));
        assert_eq!(t.palette().get(&1u8), Some(&rgb(0xff, 0xff, 0xff)));
        assert_eq!(t.palette().get(&2u8), Some(&rgb(0x80, 0x80, 0x80)));
    }

    #[test]
    fn osc_111_resets_bg_to_default() {
        let mut t = fresh();
        t.observe(b"\x1b]11;#282a36\x1b\\");
        t.observe(b"\x1b]111\x1b\\");
        assert_eq!(t.bg(), StubColors::default().bg);
    }

    #[test]
    fn osc_110_resets_fg_to_default() {
        let mut t = fresh();
        t.observe(b"\x1b]10;#aabbcc\x1b\\");
        t.observe(b"\x1b]110\x1b\\");
        assert_eq!(t.fg(), StubColors::default().fg);
    }

    #[test]
    fn osc_104_no_args_clears_palette() {
        let mut t = fresh();
        t.observe(b"\x1b]4;0;#000000;1;#ffffff\x1b\\");
        t.observe(b"\x1b]104\x1b\\");
        assert!(t.palette().is_empty());
    }

    #[test]
    fn osc_104_with_indices_clears_only_those() {
        let mut t = fresh();
        t.observe(b"\x1b]4;0;#000000;1;#ffffff;2;#808080\x1b\\");
        t.observe(b"\x1b]104;1\x1b\\");
        assert_eq!(t.palette().len(), 2);
        assert!(t.palette().contains_key(&0u8));
        assert!(t.palette().contains_key(&2u8));
    }

    #[test]
    fn ignores_non_osc_bytes() {
        let mut t = fresh();
        t.observe(b"hello world\r\n$ ");
        assert_eq!(t.bg(), StubColors::default().bg);
    }

    #[test]
    fn ignores_unknown_osc_codes() {
        let mut t = fresh();
        t.observe(b"\x1b]2;window title\x07");
        assert_eq!(t.bg(), StubColors::default().bg);
    }

    #[test]
    fn multiple_setters_in_one_chunk_apply_in_order() {
        let mut t = fresh();
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x1b]11;#111111\x1b\\");
        buf.extend_from_slice(b"some text");
        buf.extend_from_slice(b"\x1b]11;#222222\x1b\\");
        t.observe(&buf);
        assert_eq!(t.bg(), rgb(0x22, 0x22, 0x22));
    }

    #[test]
    fn malformed_osc_4_pair_does_not_apply() {
        let mut t = fresh();
        t.observe(b"\x1b]4;not-a-number;#000000\x1b\\");
        assert!(t.palette().is_empty());
    }
}
