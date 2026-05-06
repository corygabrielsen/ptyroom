//! Parse lexed lines into the [`Scene`] AST.
//!
//! The parser splits work in two passes:
//!  - Header pass: consume `Set*` (and `Version`) verbs, build a `Config`.
//!  - Body pass: consume body verbs, expanding macros (`Run`,
//!    `WaitForPrompt`) into primitive `Action`s.
//!
//! Verbs after the first body verb cannot be `Set*` (parse error).

use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use regex::bytes::Regex;

use crate::recorder::Key;

use super::ast::{Action, Config, Located, Scene, SpawnTarget};
use super::lex::{Line, Token, lex};

const SCHEMA_VERSION: u32 = 1;

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_PROMPT: &str = r"\$ ";
const DEFAULT_PER_CHAR_DWELL: Duration = Duration::from_millis(35);
const DEFAULT_PER_KEY_DWELL: Duration = Duration::from_millis(35);
// `Duration::from_mins` is unstable; use seconds and silence the lint
// that asks for a larger unit constructor.
#[allow(clippy::duration_suboptimal_units)]
const DEFAULT_MAX_RUNTIME: Duration = Duration::from_secs(240);

/// Parse a `.scene` source string into a [`Scene`].
///
/// # Errors
/// Lex errors, version mismatch, missing required header verbs, unknown
/// verb, malformed argument list — annotated with line number.
pub fn parse(source: &str) -> anyhow::Result<Scene> {
    let lines = lex(source)?;
    parse_lines(lines)
}

fn parse_lines(lines: Vec<Line>) -> anyhow::Result<Scene> {
    let mut iter = lines.into_iter().peekable();

    // 1. Version line — must be first non-empty statement.
    let version = match iter.next() {
        Some(line) if line.verb == "Version" => parse_version(&line)?,
        Some(line) => bail!(
            "scene:{}: first verb must be `Version`, found `{}`",
            line.lineno,
            line.verb,
        ),
        None => bail!("scene: empty file (no Version line)"),
    };
    if version != SCHEMA_VERSION {
        bail!("scene: unsupported version {version} (this build supports v{SCHEMA_VERSION})");
    }

    // 2. Header pass: consume Set* lines until we see a non-Set verb.
    let mut header = HeaderBuilder::default();
    while let Some(line) = iter.peek() {
        if !is_header_verb(&line.verb) {
            break;
        }
        let line = iter.next().unwrap();
        apply_header(&mut header, &line)?;
    }
    let config = header.finish()?;

    // 3. Body pass: consume body verbs, expand macros.
    let mut body: Vec<Located<Action>> = Vec::new();
    for line in iter {
        if is_header_verb(&line.verb) {
            bail!(
                "scene:{}: `{}` is a header verb but appears after the body has begun",
                line.lineno,
                line.verb,
            );
        }
        parse_body_line(&line, &config, &mut body)?;
    }

    Ok(Scene {
        version,
        config,
        body,
    })
}

fn parse_version(line: &Line) -> anyhow::Result<u32> {
    let [Token::Integer(n)] = line.args.as_slice() else {
        bail!("scene:{}: Version expects a single integer", line.lineno);
    };
    u32::try_from(*n).map_err(|_| anyhow!("scene:{}: version out of range", line.lineno))
}

fn is_header_verb(verb: &str) -> bool {
    matches!(
        verb,
        "SetCols"
            | "SetRows"
            | "SetSpawn"
            | "SetWarm"
            | "SetWarmCommand"
            | "SetCold"
            | "SetEnv"
            | "SetShellRcfile"
            | "SetMaxRuntime"
            | "SetPrompt"
            | "SetPerCharDwell"
            | "SetPerKeyDwell"
    )
}

#[derive(Default)]
struct HeaderBuilder {
    cols: Option<u16>,
    rows: Option<u16>,
    spawn: Option<SpawnTarget>,
    env: Vec<(String, String)>,
    shell_rcfile: Option<Vec<u8>>,
    max_runtime: Option<Duration>,
    prompt: Option<Regex>,
    per_char_dwell: Option<Duration>,
    per_key_dwell: Option<Duration>,
    warm_command: Option<Vec<String>>,
    /// Track which lines set the spawn target so we can report duplicates.
    spawn_set_at: Option<u32>,
}

impl HeaderBuilder {
    fn finish(self) -> anyhow::Result<Config> {
        let spawn = self.spawn.ok_or_else(|| {
            anyhow!("scene: missing process target — set one of SetSpawn / SetWarm / SetCold")
        })?;

        // Warn (without erroring) on shell_rcfile + non-Cold target —
        // the rcfile won't be applied. Future: structured warnings; for
        // now, silent acceptance matches the docs ("emits a parse warning").
        // TODO: surface as a real warning once we have a warnings channel.

        let prompt = match self.prompt {
            Some(r) => r,
            None => Regex::new(DEFAULT_PROMPT).expect("compiled default prompt"),
        };
        Ok(Config {
            cols: self.cols.unwrap_or(DEFAULT_COLS),
            rows: self.rows.unwrap_or(DEFAULT_ROWS),
            spawn,
            env: self.env,
            shell_rcfile: self.shell_rcfile,
            max_runtime: self.max_runtime.unwrap_or(DEFAULT_MAX_RUNTIME),
            prompt,
            per_char_dwell: self.per_char_dwell.unwrap_or(DEFAULT_PER_CHAR_DWELL),
            per_key_dwell: self.per_key_dwell.unwrap_or(DEFAULT_PER_KEY_DWELL),
            warm_command: self.warm_command,
        })
    }
}

fn apply_header(h: &mut HeaderBuilder, line: &Line) -> anyhow::Result<()> {
    let lineno = line.lineno;
    let ctx = || format!("scene:{lineno}: {}", line.verb);
    match line.verb.as_str() {
        "SetCols" => {
            let n = expect_one_integer(line)?;
            h.cols = Some(u16::try_from(n).with_context(ctx)?);
        }
        "SetRows" => {
            let n = expect_one_integer(line)?;
            h.rows = Some(u16::try_from(n).with_context(ctx)?);
        }
        "SetSpawn" => {
            check_spawn_unique(h, lineno, "SetSpawn")?;
            let argv = expect_strings_to_text(&line.args, lineno)?;
            if argv.is_empty() {
                bail!("scene:{lineno}: SetSpawn requires at least one argv element");
            }
            h.spawn = Some(SpawnTarget::Spawn(argv));
            h.spawn_set_at = Some(lineno);
        }
        "SetWarm" => {
            check_spawn_unique(h, lineno, "SetWarm")?;
            let name = expect_one_string_text(line)?;
            h.spawn = Some(SpawnTarget::Warm(name));
            h.spawn_set_at = Some(lineno);
        }
        "SetWarmCommand" => {
            let argv = expect_strings_to_text(&line.args, lineno)?;
            if argv.is_empty() {
                bail!("scene:{lineno}: SetWarmCommand requires at least one argv element");
            }
            h.warm_command = Some(argv);
        }
        "SetCold" => {
            check_spawn_unique(h, lineno, "SetCold")?;
            let image = expect_one_string_text(line)?;
            h.spawn = Some(SpawnTarget::Cold(image));
            h.spawn_set_at = Some(lineno);
        }
        "SetEnv" => {
            let strings = expect_strings_to_text(&line.args, lineno)?;
            let [k, v] = <[String; 2]>::try_from(strings).map_err(|_| {
                anyhow!("scene:{lineno}: SetEnv expects two string args (KEY VALUE)")
            })?;
            h.env.push((k, v));
        }
        "SetShellRcfile" => {
            h.shell_rcfile = Some(expect_one_bytes(line)?);
        }
        "SetMaxRuntime" => {
            h.max_runtime = Some(expect_one_duration(line)?);
        }
        "SetPrompt" => {
            let re = expect_one_regex(line)?;
            h.prompt = Some(re);
        }
        "SetPerCharDwell" => {
            h.per_char_dwell = Some(expect_one_duration(line)?);
        }
        "SetPerKeyDwell" => {
            h.per_key_dwell = Some(expect_one_duration(line)?);
        }
        other => bail!("scene:{lineno}: unknown header verb `{other}`"),
    }
    Ok(())
}

fn check_spawn_unique(h: &HeaderBuilder, lineno: u32, verb: &str) -> anyhow::Result<()> {
    if let Some(prev) = h.spawn_set_at {
        bail!(
            "scene:{lineno}: {verb} sets a process target, but one is already set at line {prev}"
        );
    }
    Ok(())
}

fn parse_body_line(
    line: &Line,
    config: &Config,
    out: &mut Vec<Located<Action>>,
) -> anyhow::Result<()> {
    let lineno = line.lineno;
    match line.verb.as_str() {
        "Send" => {
            let bytes = expect_one_bytes(line)?;
            out.push(Located::new(lineno, Action::Send(bytes)));
        }
        "Press" => {
            let (key, repeat, dwell, settle) = parse_press_args(line)?;
            out.push(Located::new(
                lineno,
                Action::Press {
                    key,
                    repeat,
                    dwell,
                    settle,
                },
            ));
        }
        "Type" => {
            let (text, per_char) = parse_type_args(line)?;
            out.push(Located::new(lineno, Action::Type { text, per_char }));
        }
        "WaitFor" => {
            let args = parse_waitfor_args(line)?;
            out.push(Located::new(
                lineno,
                Action::WaitFor {
                    pattern: args.pattern,
                    timeout: args.timeout,
                    label: args.label,
                    dwell: args.dwell,
                },
            ));
        }
        "WaitForPrompt" => {
            let (timeout, dwell) = parse_waitforprompt_args(line)?;
            out.push(Located::new(
                lineno,
                Action::WaitFor {
                    pattern: config.prompt.clone(),
                    timeout,
                    label: Some("prompt".into()),
                    dwell,
                },
            ));
        }
        "Sleep" => {
            let (dwell, settle) = parse_sleep_args(line)?;
            out.push(Located::new(lineno, Action::Sleep { dwell, settle }));
        }
        "Mark" => {
            let label = expect_one_string_text(line)?;
            out.push(Located::new(lineno, Action::Mark(label)));
        }
        "Present" => {
            let bytes = expect_one_bytes(line)?;
            out.push(Located::new(lineno, Action::Present(bytes)));
        }
        "PresentTyped" => {
            let (text, per_char) = parse_type_args(line)?;
            out.push(Located::new(
                lineno,
                Action::PresentTyped { text, per_char },
            ));
        }
        "Run" => {
            // Macro expansion: Type "cmd"; Press Enter; WaitFor <prompt>
            let cmd = expect_one_bytes(line)?;
            out.push(Located::new(
                lineno,
                Action::Type {
                    text: cmd,
                    per_char: None,
                },
            ));
            out.push(Located::new(
                lineno,
                Action::Press {
                    key: Key::Enter,
                    repeat: 1,
                    dwell: None,
                    settle: None,
                },
            ));
            out.push(Located::new(
                lineno,
                Action::WaitFor {
                    pattern: config.prompt.clone(),
                    timeout: None,
                    label: Some("Run prompt".into()),
                    dwell: None,
                },
            ));
        }
        other => bail!("scene:{lineno}: unknown verb `{other}`"),
    }
    Ok(())
}

fn parse_sleep_args(line: &Line) -> anyhow::Result<(Duration, Duration)> {
    let lineno = line.lineno;
    let mut iter = line.args.iter();
    let dwell_tok = iter
        .next()
        .ok_or_else(|| anyhow!("scene:{lineno}: Sleep expects a duration"))?;
    let Token::Duration(dwell) = dwell_tok else {
        bail!("scene:{lineno}: Sleep's first arg must be a duration");
    };
    let mut settle = Duration::ZERO;
    while let Some(tok) = iter.next() {
        match tok {
            Token::Ident(name) if name == "Settle" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("scene:{lineno}: Sleep Settle needs a duration"))?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: Sleep Settle expects a duration");
                };
                settle = *d;
            }
            other => bail!("scene:{lineno}: unexpected Sleep arg {other:?}"),
        }
    }
    Ok((*dwell, settle))
}

fn parse_press_args(line: &Line) -> anyhow::Result<(Key, u32, Option<Duration>, Option<Duration>)> {
    let lineno = line.lineno;
    let mut iter = line.args.iter();
    let key_tok = iter
        .next()
        .ok_or_else(|| anyhow!("scene:{lineno}: Press expects a key name"))?;
    let key_name = match key_tok {
        Token::Ident(s) => s.as_str(),
        _ => bail!("scene:{lineno}: Press's first arg must be a key name (e.g., Enter)"),
    };
    let key = key_from_name(key_name)
        .ok_or_else(|| anyhow!("scene:{lineno}: unknown key `{key_name}`"))?;
    let mut repeat = 1u32;
    let mut dwell = None;
    let mut settle = None;
    while let Some(tok) = iter.next() {
        match tok {
            Token::Integer(n) => {
                repeat =
                    u32::try_from(*n).map_err(|_| anyhow!("scene:{lineno}: repeat overflow"))?;
            }
            Token::Ident(name) if name == "Dwell" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("scene:{lineno}: Press Dwell needs a duration"))?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: Press Dwell expects a duration");
                };
                dwell = Some(*d);
            }
            Token::Ident(name) if name == "Settle" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("scene:{lineno}: Press Settle needs a duration"))?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: Press Settle expects a duration");
                };
                settle = Some(*d);
            }
            other => bail!("scene:{lineno}: unexpected Press arg {other:?}"),
        }
    }
    Ok((key, repeat, dwell, settle))
}

fn parse_type_args(line: &Line) -> anyhow::Result<(Vec<u8>, Option<Duration>)> {
    let lineno = line.lineno;
    let mut iter = line.args.iter();
    let text_tok = iter
        .next()
        .ok_or_else(|| anyhow!("scene:{lineno}: Type expects a string"))?;
    let text = bytes_from(text_tok, lineno, "Type")?;
    let mut per_char = None;
    while let Some(tok) = iter.next() {
        match tok {
            Token::Ident(name) if name == "PerChar" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("scene:{lineno}: Type PerChar needs a duration"))?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: Type PerChar expects a duration");
                };
                per_char = Some(*d);
            }
            other => bail!("scene:{lineno}: unexpected Type arg {other:?}"),
        }
    }
    Ok((text, per_char))
}

struct WaitForArgs {
    pattern: Regex,
    timeout: Option<Duration>,
    label: Option<String>,
    dwell: Option<Duration>,
}

fn parse_waitfor_args(line: &Line) -> anyhow::Result<WaitForArgs> {
    let lineno = line.lineno;
    let mut iter = line.args.iter();
    let re_tok = iter
        .next()
        .ok_or_else(|| anyhow!("scene:{lineno}: WaitFor expects a regex"))?;
    let pattern = compile_regex(re_tok, lineno, "WaitFor")?;
    let mut timeout = None;
    let mut label = None;
    let mut dwell = None;
    while let Some(tok) = iter.next() {
        match tok {
            Token::Ident(name) if name == "Timeout" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("scene:{lineno}: WaitFor Timeout needs a duration"))?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: WaitFor Timeout expects a duration");
                };
                timeout = Some(*d);
            }
            Token::Ident(name) if name == "Label" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("scene:{lineno}: WaitFor Label needs a string"))?;
                let Token::String(s) = v else {
                    bail!("scene:{lineno}: WaitFor Label expects a string");
                };
                label = Some(String::from_utf8_lossy(s).into_owned());
            }
            Token::Ident(name) if name == "Dwell" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("scene:{lineno}: WaitFor Dwell needs a duration"))?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: WaitFor Dwell expects a duration");
                };
                dwell = Some(*d);
            }
            other => bail!("scene:{lineno}: unexpected WaitFor arg {other:?}"),
        }
    }
    Ok(WaitForArgs {
        pattern,
        timeout,
        label,
        dwell,
    })
}

fn parse_waitforprompt_args(line: &Line) -> anyhow::Result<(Option<Duration>, Option<Duration>)> {
    let lineno = line.lineno;
    let mut iter = line.args.iter();
    let mut timeout = None;
    let mut dwell = None;
    while let Some(tok) = iter.next() {
        match tok {
            Token::Ident(name) if name == "Timeout" => {
                let v = iter.next().ok_or_else(|| {
                    anyhow!("scene:{lineno}: WaitForPrompt Timeout needs a duration")
                })?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: WaitForPrompt Timeout expects a duration");
                };
                timeout = Some(*d);
            }
            Token::Ident(name) if name == "Dwell" => {
                let v = iter.next().ok_or_else(|| {
                    anyhow!("scene:{lineno}: WaitForPrompt Dwell needs a duration")
                })?;
                let Token::Duration(d) = v else {
                    bail!("scene:{lineno}: WaitForPrompt Dwell expects a duration");
                };
                dwell = Some(*d);
            }
            other => bail!("scene:{lineno}: unexpected WaitForPrompt arg {other:?}"),
        }
    }
    Ok((timeout, dwell))
}

// --- helpers ---------------------------------------------------------

fn expect_one_integer(line: &Line) -> anyhow::Result<u64> {
    let [Token::Integer(n)] = line.args.as_slice() else {
        bail!(
            "scene:{}: {} expects a single integer",
            line.lineno,
            line.verb
        );
    };
    Ok(*n)
}

fn expect_one_duration(line: &Line) -> anyhow::Result<Duration> {
    let [Token::Duration(d)] = line.args.as_slice() else {
        bail!(
            "scene:{}: {} expects a single duration (e.g., 500ms)",
            line.lineno,
            line.verb
        );
    };
    Ok(*d)
}

fn expect_one_string_text(line: &Line) -> anyhow::Result<String> {
    let bytes = expect_one_bytes(line)?;
    String::from_utf8(bytes).map_err(|_| anyhow!("scene:{}: expected UTF-8 string", line.lineno))
}

fn expect_one_bytes(line: &Line) -> anyhow::Result<Vec<u8>> {
    let arg = line.args.first().ok_or_else(|| {
        anyhow!(
            "scene:{}: {} expects one string-or-heredoc arg",
            line.lineno,
            line.verb
        )
    })?;
    if line.args.len() != 1 {
        bail!(
            "scene:{}: {} expects exactly one string-or-heredoc arg",
            line.lineno,
            line.verb
        );
    }
    bytes_from(arg, line.lineno, &line.verb)
}

fn expect_one_regex(line: &Line) -> anyhow::Result<Regex> {
    let arg = line
        .args
        .first()
        .ok_or_else(|| anyhow!("scene:{}: {} expects a regex", line.lineno, line.verb))?;
    if line.args.len() != 1 {
        bail!(
            "scene:{}: {} expects a single regex arg",
            line.lineno,
            line.verb
        );
    }
    compile_regex(arg, line.lineno, &line.verb)
}

fn expect_strings_to_text(args: &[Token], lineno: u32) -> anyhow::Result<Vec<String>> {
    args.iter()
        .map(|t| match t {
            Token::String(b) => String::from_utf8(b.clone())
                .map_err(|_| anyhow!("scene:{lineno}: non-UTF-8 string arg")),
            other => bail!("scene:{lineno}: expected string, got {other:?}"),
        })
        .collect()
}

fn bytes_from(tok: &Token, lineno: u32, verb: &str) -> anyhow::Result<Vec<u8>> {
    match tok {
        Token::String(b) | Token::Heredoc(b) => Ok(b.clone()),
        other => bail!("scene:{lineno}: {verb} expected string or heredoc, got {other:?}"),
    }
}

fn compile_regex(tok: &Token, lineno: u32, verb: &str) -> anyhow::Result<Regex> {
    let Token::Regex(s) = tok else {
        bail!("scene:{lineno}: {verb} expected a /regex/, got {tok:?}");
    };
    Regex::new(s).with_context(|| format!("scene:{lineno}: {verb} regex"))
}

fn key_from_name(name: &str) -> Option<Key> {
    Some(match name {
        "Down" => Key::Down,
        "Up" => Key::Up,
        "PickerDown" => Key::PickerDown,
        "PickerUp" => Key::PickerUp,
        "Right" => Key::Right,
        "Left" => Key::Left,
        "Enter" => Key::Enter,
        "Escape" | "Esc" => Key::Escape,
        "Tab" => Key::Tab,
        "Space" => Key::Space,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Scene {
        parse(src).unwrap_or_else(|e| panic!("parse failed: {e:?}"))
    }

    #[test]
    fn minimal_scene_with_spawn() {
        let scene = p(r#"
            Version 1
            SetSpawn "bash" "-i"
            WaitForPrompt
            Run "echo hello"
        "#);
        assert_eq!(scene.version, 1);
        assert!(matches!(&scene.config.spawn, SpawnTarget::Spawn(argv) if argv == &["bash", "-i"]));
        // body: WaitForPrompt → 1 WaitFor; Run → 3 actions = 4 total.
        assert_eq!(scene.body.len(), 4);
    }

    #[test]
    fn cold_with_heredoc_rcfile() {
        let scene = p(
            "Version 1\nSetCold \"debian:12-slim\"\nSetShellRcfile <<BASH\nPS1='$ '\ncd \"$HOME\"\nBASH\nWaitForPrompt\n",
        );
        assert!(matches!(&scene.config.spawn, SpawnTarget::Cold(s) if s == "debian:12-slim"));
        let rcfile = scene.config.shell_rcfile.expect("rcfile present");
        assert!(rcfile.starts_with(b"PS1='$ '\ncd \""));
    }

    #[test]
    fn missing_version_errors() {
        let err = parse("SetSpawn \"bash\"\n").unwrap_err();
        assert!(err.to_string().contains("Version"));
    }

    #[test]
    fn missing_spawn_errors() {
        let err = parse("Version 1\nWaitForPrompt\n").unwrap_err();
        assert!(err.to_string().contains("process target"));
    }

    #[test]
    fn duplicate_spawn_errors() {
        let err = parse("Version 1\nSetSpawn \"bash\"\nSetWarm \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("already set"));
    }

    #[test]
    fn header_after_body_errors() {
        let err = parse("Version 1\nSetSpawn \"bash\"\nWaitForPrompt\nSetCols 80\n").unwrap_err();
        assert!(err.to_string().contains("body has begun"));
    }

    #[test]
    fn run_macro_expands_to_three_actions() {
        let scene = p("Version 1\nSetSpawn \"bash\"\nRun \"ls\"\n");
        // Type, Press, WaitFor
        assert_eq!(scene.body.len(), 3);
        assert!(matches!(scene.body[0].value, Action::Type { .. }));
        assert!(matches!(
            scene.body[1].value,
            Action::Press {
                key: Key::Enter,
                ..
            }
        ));
        assert!(matches!(scene.body[2].value, Action::WaitFor { .. }));
    }

    #[test]
    fn waitfor_with_timeout_and_label() {
        let scene = p(r#"
            Version 1
            SetSpawn "bash"
            WaitFor /\$ / Timeout 5s Label "echo prompt"
        "#);
        let Action::WaitFor { timeout, label, .. } = &scene.body[0].value else {
            panic!();
        };
        assert_eq!(*timeout, Some(Duration::from_secs(5)));
        assert_eq!(label.as_deref(), Some("echo prompt"));
    }

    #[test]
    fn unknown_verb_errors() {
        let err = parse("Version 1\nSetSpawn \"bash\"\nFlibbertigibbet\n").unwrap_err();
        assert!(err.to_string().contains("unknown verb"));
    }

    #[test]
    fn press_repeat_and_dwell() {
        let scene = p("Version 1\nSetSpawn \"bash\"\nPress Down 3 Dwell 50ms\n");
        let Action::Press {
            key,
            repeat,
            dwell,
            settle,
        } = &scene.body[0].value
        else {
            panic!();
        };
        assert!(matches!(key, Key::Down));
        assert_eq!(*repeat, 3);
        assert_eq!(*dwell, Some(Duration::from_millis(50)));
        assert!(settle.is_none());
    }

    #[test]
    fn press_with_settle_captures_both() {
        let scene = p("Version 1\nSetSpawn \"bash\"\nPress PickerDown 5 Dwell 50ms Settle 20ms\n");
        let Action::Press {
            key,
            repeat,
            dwell,
            settle,
        } = &scene.body[0].value
        else {
            panic!();
        };
        assert!(matches!(key, Key::PickerDown));
        assert_eq!(*repeat, 5);
        assert_eq!(*dwell, Some(Duration::from_millis(50)));
        assert_eq!(*settle, Some(Duration::from_millis(20)));
    }

    #[test]
    fn set_warm_command_captures_argv() {
        let scene =
            p("Version 1\nSetWarm \"warm-c\"\nSetWarmCommand \"my-shell\" \"-l\"\nRun \"ls\"\n");
        let argv = scene
            .config
            .warm_command
            .as_ref()
            .expect("warm_command should be set");
        assert_eq!(argv, &vec!["my-shell".to_string(), "-l".into()]);
    }

    #[test]
    fn set_warm_command_requires_at_least_one_arg() {
        let err = parse("Version 1\nSetWarm \"warm-c\"\nSetWarmCommand\nRun \"ls\"\n").unwrap_err();
        assert!(
            err.to_string().contains("at least one argv element"),
            "{err}"
        );
    }

    #[test]
    fn sleep_without_settle_defaults_to_zero() {
        let scene = p("Version 1\nSetSpawn \"bash\"\nSleep 500ms\n");
        let Action::Sleep { dwell, settle } = &scene.body[0].value else {
            panic!();
        };
        assert_eq!(*dwell, Duration::from_millis(500));
        assert_eq!(*settle, Duration::ZERO);
    }

    #[test]
    fn sleep_with_settle_captures_both() {
        let scene = p("Version 1\nSetSpawn \"bash\"\nSleep 800ms Settle 600ms\n");
        let Action::Sleep { dwell, settle } = &scene.body[0].value else {
            panic!();
        };
        assert_eq!(*dwell, Duration::from_millis(800));
        assert_eq!(*settle, Duration::from_millis(600));
    }

    #[test]
    fn present_typed_captures_text_and_per_char() {
        let scene = p("Version 1\nSetSpawn \"bash\"\nPresentTyped \"hello\" PerChar 28ms\n");
        let Action::PresentTyped { text, per_char } = &scene.body[0].value else {
            panic!("expected PresentTyped, got {:?}", scene.body[0].value);
        };
        assert_eq!(text, b"hello");
        assert_eq!(*per_char, Some(Duration::from_millis(28)));
    }

    #[test]
    fn present_typed_without_per_char_defaults_to_none() {
        let scene = p("Version 1\nSetSpawn \"bash\"\nPresentTyped \"x\"\n");
        let Action::PresentTyped { per_char, .. } = &scene.body[0].value else {
            panic!();
        };
        assert!(per_char.is_none());
    }
}
