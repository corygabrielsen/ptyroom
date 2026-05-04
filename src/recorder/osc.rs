//! OSC 11/10 query interception.
//!
//! tint queries the terminal background and foreground colors via OSC 11
//! and OSC 10 respectively. The recorder is the terminal: it watches every
//! byte tint writes, recognizes the queries, and synthesizes the canned
//! reply so the real `tint` binary runs unmodified.
//!
//! Query format: `ESC ] 11 ; ? ESC \\` (or `\x07` BEL terminator).
//! Reply format: `ESC ] 11 ; rgb:RR/GG/BB ESC \\`.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::color::HexColor;

/// Canned RGB replies for the layers tint queries (bg + fg).
#[derive(Debug, Clone, Copy)]
pub struct StubColors {
    pub bg: HexColor,
    pub fg: HexColor,
}

impl Default for StubColors {
    fn default() -> Self {
        Self {
            bg: HexColor::from_rgb(0x1a, 0x1b, 0x26),
            fg: HexColor::from_rgb(0xc0, 0xca, 0xf5),
        }
    }
}

impl StubColors {
    /// Build the OSC reply for a given OSC code (b"10" or b"11"), or `None`
    /// if the code isn't one we stub. The two recognized codes are ASCII;
    /// the function returns `None` for any other byte sequence.
    #[must_use]
    pub fn reply_for(self, code: &[u8]) -> Option<Vec<u8>> {
        let (code_str, color) = match code {
            b"11" => ("11", self.bg),
            b"10" => ("10", self.fg),
            _ => return None,
        };
        Some(
            format!(
                "\x1b]{code_str};rgb:{:02x}/{:02x}/{:02x}\x1b\\",
                color.r(),
                color.g(),
                color.b(),
            )
            .into_bytes(),
        )
    }
}

/// Lazily-compiled regex matching `ESC ] 1[01] ; ? ( ESC \\ | BEL )`.
/// Single static instance so the hot replay loop pays compile cost once.
fn query_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\x1b\](10|11);\?(?:\x1b\\|\x07)").unwrap())
}

/// Matches `ESC ] 1[01] ; <color> ( ESC \\ | BEL )` where `<color>` is either
/// `#RRGGBB` (tint's wire format) or `rgb:R/G/B` (xterm-spec form some
/// clients use). Accepts 1-4 hex digits per channel for the rgb: form.
fn setter_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = concat!(
            r"\x1b\](10|11);",
            r"(#[0-9a-fA-F]{6}|rgb:[0-9a-fA-F]{1,4}/[0-9a-fA-F]{1,4}/[0-9a-fA-F]{1,4})",
            r"(?:\x1b\\|\x07)",
        );
        Regex::new(pat).unwrap()
    })
}

/// Scan `chunk` for OSC 10/11 setter sequences. Returns `(code, color)`
/// pairs in chunk order so callers can apply them as a sequence of state
/// updates.
#[must_use]
pub fn setters_in_chunk(chunk: &[u8]) -> Vec<(u8, HexColor)> {
    setter_re()
        .captures_iter(chunk)
        .filter_map(|cap| {
            let code_b = cap.get(1)?.as_bytes();
            let code: u8 = match code_b {
                b"10" => 10,
                b"11" => 11,
                _ => return None,
            };
            let color_str = std::str::from_utf8(cap.get(2)?.as_bytes()).ok()?;
            let color = HexColor::parse(color_str)?;
            Some((code, color))
        })
        .collect()
}

/// Scan `chunk` for OSC queries and emit the canned replies.
pub fn replies_for_chunk(chunk: &[u8], stubs: StubColors) -> Vec<Vec<u8>> {
    query_re()
        .captures_iter(chunk)
        .filter_map(|cap| {
            let code = cap.get(1)?.as_bytes();
            stubs.reply_for(code)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_osc_11_st_terminated() {
        let q = b"\x1b]11;?\x1b\\";
        let stubs = StubColors::default();
        let replies = replies_for_chunk(q, stubs);
        assert_eq!(replies.len(), 1);
        let s = std::str::from_utf8(&replies[0]).unwrap();
        assert!(s.starts_with("\x1b]11;rgb:1a/1b/26"));
    }

    #[test]
    fn matches_osc_10_bel_terminated() {
        let q = b"\x1b]10;?\x07";
        let replies = replies_for_chunk(q, StubColors::default());
        assert_eq!(replies.len(), 1);
        let s = std::str::from_utf8(&replies[0]).unwrap();
        assert!(s.starts_with("\x1b]10;rgb:c0/ca/f5"));
    }

    #[test]
    fn ignores_non_query_osc() {
        // OSC 11 SET (not query) — has a color arg, not `?`
        let set = b"\x1b]11;rgb:00/00/00\x1b\\";
        assert!(replies_for_chunk(set, StubColors::default()).is_empty());
    }

    #[test]
    fn handles_multiple_queries_in_one_chunk() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x1b]11;?\x1b\\");
        buf.extend_from_slice(b"prompt");
        buf.extend_from_slice(b"\x1b]10;?\x1b\\");
        let replies = replies_for_chunk(&buf, StubColors::default());
        assert_eq!(replies.len(), 2);
    }

    #[test]
    fn ignores_unknown_codes() {
        // OSC 4 (palette) query — we don't stub it
        let q = b"\x1b]4;?\x1b\\";
        assert!(replies_for_chunk(q, StubColors::default()).is_empty());
    }

    #[test]
    fn setter_hash_form_st_terminated() {
        let s = b"\x1b]11;#282a36\x1b\\";
        let setters = setters_in_chunk(s);
        assert_eq!(setters.len(), 1);
        assert_eq!(setters[0].0, 11);
        assert_eq!(setters[0].1, HexColor::from_rgb(0x28, 0x2a, 0x36));
    }

    #[test]
    fn setter_rgb_form_bel_terminated() {
        let s = b"\x1b]10;rgb:c0/ca/f5\x07";
        let setters = setters_in_chunk(s);
        assert_eq!(setters.len(), 1);
        assert_eq!(setters[0].0, 10);
        assert_eq!(setters[0].1, HexColor::from_rgb(0xc0, 0xca, 0xf5));
    }

    #[test]
    fn setter_ignores_query() {
        let q = b"\x1b]11;?\x1b\\";
        assert!(setters_in_chunk(q).is_empty());
    }

    #[test]
    fn setter_finds_multiple_in_one_chunk() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x1b]11;#282a36\x1b\\");
        buf.extend_from_slice(b"some output");
        buf.extend_from_slice(b"\x1b]10;#f8f8f2\x1b\\");
        let setters = setters_in_chunk(&buf);
        assert_eq!(setters.len(), 2);
        assert_eq!(setters[0].0, 11);
        assert_eq!(setters[1].0, 10);
    }
}
