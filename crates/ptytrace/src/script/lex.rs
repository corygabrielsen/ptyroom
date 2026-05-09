//! Lexer for script files.
//!
//! Line-oriented: each statement is one logical line, except heredocs
//! (which span the lines from `<<NAME` through the matching `NAME`
//! terminator). Comments start with `#` and run to end of line.

use std::time::Duration;

use anyhow::{Context, anyhow, bail};

/// One logical line: a verb plus its argument tokens.
#[derive(Debug, Clone)]
pub struct Line {
    pub lineno: u32,
    pub verb: String,
    pub args: Vec<Token>,
}

/// Argument tokens. The verb name itself is on `Line.verb`.
#[derive(Debug, Clone)]
pub enum Token {
    /// Bare identifier (key names like `Enter`, keyword args like `Timeout`).
    Ident(String),
    /// `"..."` with C escapes processed.
    String(Vec<u8>),
    /// `<<NAME ... NAME` — verbatim bytes between the markers.
    Heredoc(Vec<u8>),
    /// `/.../` — raw regex source, not yet compiled.
    Regex(String),
    /// Bare integer literal.
    Integer(u64),
    /// Integer + unit (`ms`, `s`, `m`).
    Duration(Duration),
}

/// Tokenize the full source into a sequence of logical lines.
///
/// # Errors
/// Lexical errors (bad string escape, unterminated regex, unterminated
/// heredoc, unknown character) annotated with line number.
pub fn lex(source: &str) -> anyhow::Result<Vec<Line>> {
    let raw_lines: Vec<&str> = source.lines().collect();
    let mut lines: Vec<Line> = Vec::new();
    let mut i = 0;
    while i < raw_lines.len() {
        let lineno = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let raw = raw_lines[i];
        let trimmed = strip_comment_and_trim(raw);
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        let (verb, rest) = split_verb(trimmed)
            .ok_or_else(|| anyhow!("script:{lineno}: line does not start with a verb"))?;
        let mut args = Vec::new();
        let mut chars = rest.chars().peekable();

        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
                continue;
            }
            // Trailing comment after args — stop tokenizing this line.
            if c == '#' {
                break;
            }
            let token = match c {
                '"' => parse_string(&mut chars)
                    .with_context(|| format!("script:{lineno}: in string"))?,
                '/' => {
                    parse_regex(&mut chars).with_context(|| format!("script:{lineno}: in regex"))?
                }
                '<' => {
                    let name = parse_heredoc_marker(&mut chars)
                        .with_context(|| format!("script:{lineno}: in heredoc marker"))?;
                    let (content, lines_consumed) = read_heredoc(&raw_lines[i + 1..], &name)
                        .with_context(|| format!("script:{lineno}: heredoc {name}"))?;
                    i += lines_consumed;
                    Token::Heredoc(content)
                }
                c if c.is_ascii_digit() => parse_number_or_duration(&mut chars)
                    .with_context(|| format!("script:{lineno}: in number"))?,
                c if is_ident_start(c) => parse_ident(&mut chars),
                _ => bail!("script:{lineno}: unexpected character {c:?}"),
            };
            args.push(token);
        }

        lines.push(Line { lineno, verb, args });
        i += 1;
    }
    Ok(lines)
}

fn strip_comment_and_trim(line: &str) -> &str {
    // Strip comments only at start-of-line or after whitespace; we don't
    // try to detect mid-string `#` because the lexer below handles that
    // when scanning args.
    let mut end = line.len();
    let mut in_string = false;
    for (i, c) in line.char_indices() {
        if c == '"' {
            in_string = !in_string;
        }
        if c == '#' && !in_string {
            // Only treat as comment if # is at start or preceded by whitespace
            if i == 0
                || line[..i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace)
            {
                end = i;
                break;
            }
        }
    }
    line[..end].trim()
}

fn split_verb(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let verb = &s[..end];
    if !is_ident_start(verb.chars().next()?) {
        return None;
    }
    Some((verb.to_string(), &s[end..]))
}

fn parse_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> anyhow::Result<Token> {
    chars.next(); // consume opening "
    let mut bytes = Vec::new();
    loop {
        let c = chars.next().ok_or_else(|| anyhow!("unterminated string"))?;
        match c {
            '"' => return Ok(Token::String(bytes)),
            '\\' => {
                let esc = chars.next().ok_or_else(|| anyhow!("dangling backslash"))?;
                match esc {
                    'n' => bytes.push(b'\n'),
                    'r' => bytes.push(b'\r'),
                    't' => bytes.push(b'\t'),
                    '\\' => bytes.push(b'\\'),
                    '"' => bytes.push(b'"'),
                    'e' => bytes.push(0x1b),
                    '0' => bytes.push(0),
                    'x' => {
                        let h1 = chars
                            .next()
                            .ok_or_else(|| anyhow!("truncated \\x escape"))?;
                        let h2 = chars
                            .next()
                            .ok_or_else(|| anyhow!("truncated \\x escape"))?;
                        let hi = h1
                            .to_digit(16)
                            .ok_or_else(|| anyhow!("bad \\x hex digit {h1:?}"))?;
                        let lo = h2
                            .to_digit(16)
                            .ok_or_else(|| anyhow!("bad \\x hex digit {h2:?}"))?;
                        bytes.push(u8::try_from(hi * 16 + lo).unwrap());
                    }
                    other => bail!("unknown escape \\{other}"),
                }
            }
            other => {
                let mut buf = [0u8; 4];
                let s = other.encode_utf8(&mut buf);
                bytes.extend_from_slice(s.as_bytes());
            }
        }
    }
}

fn parse_regex(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> anyhow::Result<Token> {
    chars.next(); // consume opening /
    let mut s = String::new();
    loop {
        let c = chars.next().ok_or_else(|| anyhow!("unterminated regex"))?;
        match c {
            '/' => return Ok(Token::Regex(s)),
            '\\' => {
                // Preserve the escape verbatim so the regex engine sees it.
                let esc = chars
                    .next()
                    .ok_or_else(|| anyhow!("dangling backslash in regex"))?;
                s.push('\\');
                s.push(esc);
            }
            other => s.push(other),
        }
    }
}

fn parse_heredoc_marker(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> anyhow::Result<String> {
    chars.next(); // consume first <
    let next = chars
        .next()
        .ok_or_else(|| anyhow!("expected `<<` for heredoc"))?;
    if next != '<' {
        bail!("expected `<<`, found `<{next}`");
    }
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if is_ident_continue(c) {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if name.is_empty() {
        bail!("heredoc marker `<<` followed by no identifier");
    }
    if !is_ident_start(name.chars().next().unwrap()) {
        bail!("heredoc marker must start with a letter, got {name:?}");
    }
    Ok(name)
}

fn read_heredoc(lookahead: &[&str], name: &str) -> anyhow::Result<(Vec<u8>, usize)> {
    let mut content = Vec::new();
    for (i, line) in lookahead.iter().enumerate() {
        if line.trim_end() == name {
            return Ok((content, i + 1));
        }
        content.extend_from_slice(line.as_bytes());
        content.push(b'\n');
    }
    bail!("heredoc {name} unterminated");
}

fn parse_number_or_duration(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> anyhow::Result<Token> {
    let mut digits = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(c);
            chars.next();
        } else {
            break;
        }
    }
    let n: u64 = digits.parse().context("integer overflow")?;

    // Read optional unit
    let mut unit = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphabetic() {
            unit.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if unit.is_empty() {
        return Ok(Token::Integer(n));
    }
    let dur = match unit.as_str() {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        other => bail!("unknown duration unit {other:?} (expected ms / s / m)"),
    };
    Ok(Token::Duration(dur))
}

fn parse_ident(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Token {
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if is_ident_continue(c) {
            s.push(c);
            chars.next();
        } else {
            break;
        }
    }
    Token::Ident(s)
}

const fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

const fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source() {
        let lines = lex("").unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn comments_and_blanks_stripped() {
        let src = "# comment\n\n  # another\n\n";
        let lines = lex(src).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn version_line() {
        let lines = lex("Version 1\n").unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].verb, "Version");
        assert!(matches!(lines[0].args[0], Token::Integer(1)));
    }

    #[test]
    fn string_with_escapes() {
        let lines = lex(r#"Type "hello\n\xff""#).unwrap();
        let Token::String(bytes) = &lines[0].args[0] else {
            panic!("expected String");
        };
        assert_eq!(bytes, &b"hello\n\xff".to_vec());
    }

    #[test]
    fn regex_token() {
        let lines = lex(r"WaitFor /\$ /").unwrap();
        let Token::Regex(s) = &lines[0].args[0] else {
            panic!("expected Regex");
        };
        assert_eq!(s, r"\$ ");
    }

    #[test]
    fn duration_units() {
        let lines = lex("Sleep 500ms\nSleep 2s\nSleep 1m\n").unwrap();
        assert_eq!(lines.len(), 3);
        let extract = |tok: &Token| match tok {
            Token::Duration(d) => *d,
            _ => panic!("expected Duration"),
        };
        assert_eq!(extract(&lines[0].args[0]), Duration::from_millis(500));
        assert_eq!(extract(&lines[1].args[0]), Duration::from_secs(2));
        // 1m parses as 60 seconds; comparing to from_secs(60) trips
        // an "use larger unit" lint on Duration construction, so check
        // the seconds count directly.
        assert_eq!(extract(&lines[2].args[0]).as_secs(), 60);
    }

    #[test]
    fn heredoc_captures_lines() {
        let src = "Present <<EOF\nline 1\nline 2\nEOF\n";
        let lines = lex(src).unwrap();
        assert_eq!(lines.len(), 1);
        let Token::Heredoc(content) = &lines[0].args[0] else {
            panic!("expected Heredoc");
        };
        assert_eq!(content, b"line 1\nline 2\n");
    }

    #[test]
    fn heredoc_unterminated_errors() {
        let src = "Present <<EOF\nstuff\n";
        let err = lex(src).unwrap_err();
        assert!(err.to_string().contains("heredoc"));
    }

    #[test]
    fn keyword_args_after_positional() {
        let lines = lex(r#"WaitFor /\$ / Timeout 5s Label "echo""#).unwrap();
        assert_eq!(lines[0].args.len(), 5);
        assert!(matches!(lines[0].args[0], Token::Regex(_)));
        assert!(matches!(&lines[0].args[1], Token::Ident(s) if s == "Timeout"));
        assert!(matches!(lines[0].args[2], Token::Duration(_)));
        assert!(matches!(&lines[0].args[3], Token::Ident(s) if s == "Label"));
        assert!(matches!(&lines[0].args[4], Token::String(s) if s == b"echo"));
    }

    #[test]
    fn trailing_comment_stripped() {
        let lines = lex("Sleep 1s   # wait a bit\n").unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].args.len(), 1);
    }

    #[test]
    fn hash_inside_string_is_not_comment() {
        let lines = lex(r#"Type "size #1""#).unwrap();
        let Token::String(bytes) = &lines[0].args[0] else {
            panic!();
        };
        assert_eq!(bytes, b"size #1");
    }
}
