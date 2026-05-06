//! Scene AST — the parsed, expanded form of a `.scene` file.
//!
//! `Run` and `WaitForPrompt` macros are expanded by the parser before
//! reaching the AST, so executors only see primitive verbs.

use std::time::Duration;

use regex::bytes::Regex;

use crate::recorder::Key;

/// A parsed scene file, ready for execution.
#[derive(Debug)]
pub struct Scene {
    pub version: u32,
    pub config: Config,
    pub body: Vec<Located<Action>>,
}

/// Source position attached to an AST node, used for line-numbered errors.
#[derive(Debug, Clone, Copy)]
pub struct Located<T> {
    pub line: u32,
    pub value: T,
}

impl<T> Located<T> {
    pub const fn new(line: u32, value: T) -> Self {
        Self { line, value }
    }
}

/// Header configuration extracted from `Set*` verbs.
#[derive(Debug)]
pub struct Config {
    pub cols: u16,
    pub rows: u16,
    pub spawn: SpawnTarget,
    pub env: Vec<(String, String)>,
    pub shell_rcfile: Option<Vec<u8>>,
    pub max_runtime: Duration,
    pub prompt: Regex,
    pub per_char_dwell: Duration,
    pub per_key_dwell: Duration,
    /// Optional command-line for the warm-container exec, set via
    /// `SetWarmCommand`. Meaningful only with `SpawnTarget::Warm`;
    /// with Spawn or Cold this is silently ignored. When `None`, the
    /// recorder's default (`bash -i`) is used.
    pub warm_command: Option<Vec<String>>,
}

/// Process target — exactly one of these is required in the header.
#[derive(Debug, Clone)]
pub enum SpawnTarget {
    Spawn(Vec<String>),
    Warm(String),
    Cold(String),
}

/// Body verbs after macro expansion.
#[derive(Debug)]
pub enum Action {
    // Class A — PTY side-effect (no cast event)
    Send(Vec<u8>),
    Press {
        key: Key,
        repeat: u32,
        dwell: Option<Duration>,
    },
    Type {
        text: Vec<u8>,
        per_char: Option<Duration>,
    },

    // Class B — event-producing
    WaitFor {
        pattern: Regex,
        timeout: Option<Duration>,
        label: Option<String>,
    },
    Sleep(Duration),
    Mark(String),
    Present(Vec<u8>),
}
