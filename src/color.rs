//! Color types for tint-recorder.
//!
//! [`HexColor`] is a 24-bit RGB color stored as a packed `u32` (`0x00RRGGBB`).
//! Total parsers and total constructors — every public function returns either
//! a valid color or `None`/`Err`, never panics.
//!
//! [`CellColor`] is the algebraic data type a single terminal cell can carry:
//! `Default` (terminal default), `Rgb(HexColor)` (24-bit truecolor), or
//! `Palette { idx, fallback }` (xterm 256-color palette index, with the OSC 4
//! override at capture time recorded as a fallback).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 24-bit RGB color. Internal representation is the packed integer
/// `0x00RRGGBB`; the high byte is always zero.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct HexColor(u32);

impl HexColor {
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        HexColor(((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    pub const fn r(self) -> u8 { ((self.0 >> 16) & 0xff) as u8 }
    pub const fn g(self) -> u8 { ((self.0 >> 8) & 0xff) as u8 }
    pub const fn b(self) -> u8 { (self.0 & 0xff) as u8 }

    pub const fn rgb(self) -> (u8, u8, u8) { (self.r(), self.g(), self.b()) }

    /// Parse `#rrggbb`, `rrggbb`, or the xterm OSC color reply form
    /// `rgb:RR[RR]/GG[GG]/BB[BB]`. Case-insensitive. Returns `None` on any
    /// malformed input. Total — never panics.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("rgb:") {
            return Self::parse_xterm_rgb(rest);
        }
        let hex = s.strip_prefix('#').unwrap_or(s);
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Self::from_rgb(r, g, b))
    }

    /// xterm OSC color reply: `RR[RR]/GG[GG]/BB[BB]` (1-4 hex digits per
    /// component; we keep the high byte). All known terminals emit 2 or 4
    /// digits; we accept 1-4 defensively.
    fn parse_xterm_rgb(s: &str) -> Option<Self> {
        let mut it = s.split('/');
        let r = parse_xterm_component(it.next()?)?;
        let g = parse_xterm_component(it.next()?)?;
        let b = parse_xterm_component(it.next()?)?;
        if it.next().is_some() { return None; }
        Some(Self::from_rgb(r, g, b))
    }
}

fn parse_xterm_component(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 4 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    // Take the high two hex digits (most significant) as the byte value.
    // For 1-digit input, scale to 8 bits by repeating (e.g. "f" → 0xff).
    let high = match s.len() {
        1 => {
            let d = u8::from_str_radix(s, 16).ok()?;
            (d << 4) | d
        }
        _ => u8::from_str_radix(&s[..2], 16).ok()?,
    };
    Some(high)
}

impl fmt::Display for HexColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r(), self.g(), self.b())
    }
}

impl fmt::Debug for HexColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HexColor({})", self)
    }
}

impl FromStr for HexColor {
    type Err = ParseHexColorError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        HexColor::parse(s).ok_or(ParseHexColorError)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseHexColorError;

impl fmt::Display for ParseHexColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid hex color (expected #rrggbb or rgb:RR/GG/BB)")
    }
}

impl std::error::Error for ParseHexColorError {}

impl Serialize for HexColor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        HexColor::parse(&s).ok_or_else(|| serde::de::Error::custom(
            format!("invalid hex color: {s:?}"),
        ))
    }
}

// ─────────────── CellColor ───────────────

/// What a single terminal cell's foreground or background color is.
///
/// Encoded in snapshot JSON as:
/// - `null`                        → [`CellColor::Default`]
/// - `"#rrggbb"`                   → [`CellColor::Rgb`]
/// - `{ "palette": N, "fallback": "#rrggbb" | null }` → [`CellColor::Palette`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellColor {
    Default,
    Rgb(HexColor),
    Palette { idx: u8, fallback: Option<HexColor> },
}

impl CellColor {
    /// Resolve this color to a concrete RGB value, given the snapshot's
    /// terminal default and OSC 4 palette overrides. Falls back to the
    /// xterm default 16-color palette for indices 0-15 if no override is
    /// available, then to `default_for_layer` as the ultimate fallback.
    pub fn resolve(
        &self,
        default_for_layer: HexColor,
        palette: &PaletteOverrides,
    ) -> HexColor {
        match self {
            CellColor::Default => default_for_layer,
            CellColor::Rgb(c) => *c,
            CellColor::Palette { idx, fallback } => {
                if let Some(fb) = fallback { return *fb; }
                if let Some(over) = palette.get(*idx) { return over; }
                if (*idx as usize) < DEFAULT_ANSI_16.len() {
                    return DEFAULT_ANSI_16[*idx as usize];
                }
                default_for_layer
            }
        }
    }
}

impl Serialize for CellColor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CellColor::Default => s.serialize_none(),
            CellColor::Rgb(c) => c.serialize(s),
            CellColor::Palette { idx, fallback } => {
                use serde::ser::SerializeStruct;
                let mut st = s.serialize_struct("Palette", 2)?;
                st.serialize_field("palette", idx)?;
                st.serialize_field("fallback", fallback)?;
                st.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for CellColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept null, string, or { palette, fallback } object.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Hex(String),
            Palette { palette: u8, fallback: Option<HexColor> },
        }
        match Option::<Repr>::deserialize(d)? {
            None => Ok(CellColor::Default),
            Some(Repr::Hex(s)) => HexColor::parse(&s)
                .map(CellColor::Rgb)
                .ok_or_else(|| serde::de::Error::custom(
                    format!("invalid cell color string: {s:?}"),
                )),
            Some(Repr::Palette { palette, fallback }) => Ok(CellColor::Palette {
                idx: palette,
                fallback,
            }),
        }
    }
}

// ─────────────── PaletteOverrides ───────────────

/// OSC 4 palette index → color overrides captured during a cast replay.
/// Stored as JSON objects with stringified integer keys (`{"4": "#aabbcc"}`)
/// — that's how snapshot.ts emits them — so we wrap a plain `Vec` and parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteOverrides(Vec<(u8, HexColor)>);

impl PaletteOverrides {
    pub fn new() -> Self { Self(Vec::new()) }

    pub fn get(&self, idx: u8) -> Option<HexColor> {
        self.0.iter().find_map(|(k, v)| (*k == idx).then_some(*v))
    }

    pub fn set(&mut self, idx: u8, color: HexColor) {
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| *k == idx) {
            slot.1 = color;
        } else {
            self.0.push((idx, color));
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (u8, HexColor)> + '_ {
        self.0.iter().copied()
    }

    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

impl Serialize for PaletteOverrides {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (idx, color) in &self.0 {
            m.serialize_entry(&idx.to_string(), color)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for PaletteOverrides {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // JSON object keyed by stringified u8.
        let map: std::collections::BTreeMap<String, HexColor> =
            std::collections::BTreeMap::deserialize(d)?;
        let mut out = PaletteOverrides::new();
        for (k, v) in map {
            let idx: u8 = k.parse().map_err(|_| serde::de::Error::custom(
                format!("invalid palette index key: {k:?}"),
            ))?;
            out.set(idx, v);
        }
        Ok(out)
    }
}

// ─────────────── Default ANSI 16-color palette (xterm) ───────────────

/// The xterm "rxvt" default 16-color palette. Used as a last-resort fallback
/// when a [`CellColor::Palette`] reference has no inline fallback and no OSC 4
/// override. Mirrors the values used by `paint.py`'s `DEFAULT_ANSI`.
pub const DEFAULT_ANSI_16: [HexColor; 16] = [
    HexColor::from_rgb(0x00, 0x00, 0x00), HexColor::from_rgb(0xcd, 0x00, 0x00),
    HexColor::from_rgb(0x00, 0xcd, 0x00), HexColor::from_rgb(0xcd, 0xcd, 0x00),
    HexColor::from_rgb(0x00, 0x00, 0xee), HexColor::from_rgb(0xcd, 0x00, 0xcd),
    HexColor::from_rgb(0x00, 0xcd, 0xcd), HexColor::from_rgb(0xe5, 0xe5, 0xe5),
    HexColor::from_rgb(0x7f, 0x7f, 0x7f), HexColor::from_rgb(0xff, 0x00, 0x00),
    HexColor::from_rgb(0x00, 0xff, 0x00), HexColor::from_rgb(0xff, 0xff, 0x00),
    HexColor::from_rgb(0x5c, 0x5c, 0xff), HexColor::from_rgb(0xff, 0x00, 0xff),
    HexColor::from_rgb(0x00, 0xff, 0xff), HexColor::from_rgb(0xff, 0xff, 0xff),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_with_hash() {
        assert_eq!(HexColor::parse("#aabbcc"), Some(HexColor::from_rgb(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn parses_hex_without_hash() {
        assert_eq!(HexColor::parse("AABBCC"), Some(HexColor::from_rgb(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn rejects_short_hex() {
        assert_eq!(HexColor::parse("#abc"), None);
    }

    #[test]
    fn rejects_non_hex() {
        assert_eq!(HexColor::parse("#zzzzzz"), None);
    }

    #[test]
    fn parses_xterm_rgb_2digit() {
        assert_eq!(HexColor::parse("rgb:aa/bb/cc"), Some(HexColor::from_rgb(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn parses_xterm_rgb_4digit_keeps_high_byte() {
        assert_eq!(HexColor::parse("rgb:abcd/ef01/2345"), Some(HexColor::from_rgb(0xab, 0xef, 0x23)));
    }

    #[test]
    fn parses_xterm_rgb_1digit_repeats() {
        assert_eq!(HexColor::parse("rgb:f/0/a"), Some(HexColor::from_rgb(0xff, 0x00, 0xaa)));
    }

    #[test]
    fn rejects_xterm_rgb_too_few_components() {
        assert_eq!(HexColor::parse("rgb:aa/bb"), None);
    }

    #[test]
    fn formats_as_lowercase_hex() {
        assert_eq!(HexColor::from_rgb(0xab, 0xcd, 0xef).to_string(), "#abcdef");
    }

    #[test]
    fn cell_color_default_resolves_to_layer_default() {
        let c = CellColor::Default;
        let dflt = HexColor::from_rgb(0x12, 0x34, 0x56);
        let overrides = PaletteOverrides::new();
        assert_eq!(c.resolve(dflt, &overrides), dflt);
    }

    #[test]
    fn cell_color_palette_uses_inline_fallback_first() {
        let inline = HexColor::from_rgb(0xaa, 0xaa, 0xaa);
        let c = CellColor::Palette { idx: 1, fallback: Some(inline) };
        let mut overrides = PaletteOverrides::new();
        overrides.set(1, HexColor::from_rgb(0xbb, 0xbb, 0xbb));
        assert_eq!(c.resolve(HexColor::from_rgb(0,0,0), &overrides), inline);
    }

    #[test]
    fn cell_color_palette_uses_overrides_when_no_inline_fallback() {
        let over = HexColor::from_rgb(0xbb, 0xbb, 0xbb);
        let c = CellColor::Palette { idx: 1, fallback: None };
        let mut overrides = PaletteOverrides::new();
        overrides.set(1, over);
        assert_eq!(c.resolve(HexColor::from_rgb(0,0,0), &overrides), over);
    }

    #[test]
    fn cell_color_palette_falls_back_to_default_ansi() {
        let c = CellColor::Palette { idx: 1, fallback: None };
        let overrides = PaletteOverrides::new();
        assert_eq!(c.resolve(HexColor::from_rgb(0,0,0), &overrides), DEFAULT_ANSI_16[1]);
    }

    #[test]
    fn cell_color_palette_high_idx_falls_back_to_layer_default() {
        let c = CellColor::Palette { idx: 200, fallback: None };
        let overrides = PaletteOverrides::new();
        let dflt = HexColor::from_rgb(0x12, 0x34, 0x56);
        assert_eq!(c.resolve(dflt, &overrides), dflt);
    }

    #[test]
    fn json_round_trip_default() {
        let c = CellColor::Default;
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, "null");
        assert_eq!(serde_json::from_str::<CellColor>(&s).unwrap(), c);
    }

    #[test]
    fn json_round_trip_rgb() {
        let c = CellColor::Rgb(HexColor::from_rgb(0xaa, 0xbb, 0xcc));
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, "\"#aabbcc\"");
        assert_eq!(serde_json::from_str::<CellColor>(&s).unwrap(), c);
    }

    #[test]
    fn json_round_trip_palette_with_fallback() {
        let c = CellColor::Palette {
            idx: 7,
            fallback: Some(HexColor::from_rgb(0x11, 0x22, 0x33)),
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: CellColor = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn palette_overrides_json_uses_string_keys() {
        let mut p = PaletteOverrides::new();
        p.set(4, HexColor::from_rgb(0xaa, 0xbb, 0xcc));
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"4\""));
    }
}
