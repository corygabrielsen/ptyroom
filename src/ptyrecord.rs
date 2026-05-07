//! `.ptyrecord` bundle format.
//!
//! A ptyrecord is a self-contained JSON artifact that embeds:
//! - the raw `.ptytrace` bytes,
//! - the rendered media bytes,
//! - optional witness metadata,
//! - selectable text projections derived from replaying the trace.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::frame::Frame;
use crate::frame_replay::replay;
use crate::pty::StubColors;
use crate::trace::{EventKind, Trace};
use crate::witness::{Witness, sha256_hex};

pub const PTYRECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtyRecord {
    pub version: u32,
    pub trace: EmbeddedFile,
    pub media: EmbeddedFile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<Witness>,
    pub transcript: Transcript,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedFile {
    pub path: String,
    pub media_type: String,
    pub sha256: String,
    pub encoding: Encoding,
    pub bytes_base64: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Base64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    /// Plain text extracted from all output events, with ANSI/OSC
    /// control sequences removed. This is the copy/search view.
    pub plain_text: String,
    /// Visible text after each output event. This is the synchronized
    /// current-frame view for UI components.
    pub frames: Vec<TextFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextFrame {
    pub time_s: f64,
    pub rows: Vec<String>,
}

impl PtyRecord {
    /// Build a ptyrecord from already-written trace and media files.
    ///
    /// # Errors
    /// IO failure, malformed trace bytes, or trace replay failure.
    pub fn from_paths(
        trace_path: impl AsRef<Path>,
        media_path: impl AsRef<Path>,
        witness: Option<&Witness>,
    ) -> anyhow::Result<Self> {
        let trace_path = trace_path.as_ref();
        let media_path = media_path.as_ref();
        let trace_bytes = std::fs::read(trace_path)?;
        let media_bytes = std::fs::read(media_path)?;
        let parsed_trace = parse_trace_bytes(&trace_bytes)?;
        let transcript = Transcript::from_trace(&parsed_trace)?;
        let trace = EmbeddedFile::new(
            file_name(trace_path),
            "application/x-ptytrace+jsonl",
            &trace_bytes,
        );
        let media = EmbeddedFile::new(
            file_name(media_path),
            media_type_for(media_path),
            &media_bytes,
        );
        ensure_supported_media_type(&media.media_type)?;
        if let Some(witness) = witness {
            if witness.trace_sha256 != trace.sha256 {
                anyhow::bail!("witness trace hash does not match embedded trace");
            }
            if witness.output_sha256 != media.sha256 {
                anyhow::bail!("witness output hash does not match embedded media");
            }
        }

        Ok(Self {
            version: PTYRECORD_VERSION,
            trace,
            media,
            witness: witness.cloned(),
            transcript,
        })
    }

    /// Read a ptyrecord JSON file.
    ///
    /// # Errors
    /// IO or JSON parse failure, unsupported schema version, invalid
    /// embedded hashes, or projections that do not match the trace.
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path.as_ref())?;
        let record: Self = serde_json::from_slice(&bytes)?;
        record.validate()?;
        Ok(record)
    }

    /// Validate every embedded consistency claim.
    ///
    /// # Errors
    /// Unsupported schema version, invalid base64, hash mismatch,
    /// malformed trace, stale text projection, or witness mismatch.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.version != PTYRECORD_VERSION {
            anyhow::bail!(
                "unsupported ptyrecord version {} (this build supports v{PTYRECORD_VERSION})",
                self.version
            );
        }
        let trace_bytes = self.trace.decode()?;
        let media_bytes = self.media.decode()?;
        ensure_supported_media_type(&self.media.media_type)?;
        verify_embedded_hash("trace", &trace_bytes, &self.trace.sha256)?;
        verify_embedded_hash("media", &media_bytes, &self.media.sha256)?;

        let trace = parse_trace_bytes(&trace_bytes)?;
        let transcript = Transcript::from_trace(&trace)?;
        if transcript != self.transcript {
            anyhow::bail!("transcript projection does not match embedded trace");
        }

        if let Some(witness) = &self.witness {
            if witness.trace_sha256 != self.trace.sha256 {
                anyhow::bail!("witness trace hash does not match embedded trace");
            }
            if witness.output_sha256 != self.media.sha256 {
                anyhow::bail!("witness output hash does not match embedded media");
            }
        }

        Ok(())
    }

    /// Write a ptyrecord JSON file.
    ///
    /// # Errors
    /// IO or JSON serialization failure.
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

impl EmbeddedFile {
    fn new(path: String, media_type: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            path,
            media_type: media_type.into(),
            sha256: sha256_hex(bytes),
            encoding: Encoding::Base64,
            bytes_base64: BASE64.encode(bytes),
        }
    }

    /// Decode the embedded bytes.
    ///
    /// # Errors
    /// The encoded payload is not valid base64.
    pub fn decode(&self) -> anyhow::Result<Vec<u8>> {
        match self.encoding {
            Encoding::Base64 => BASE64
                .decode(&self.bytes_base64)
                .map_err(|err| anyhow::anyhow!("invalid base64 for {}: {err}", self.path)),
        }
    }
}

impl Transcript {
    /// Derive selectable text from trace replay.
    ///
    /// # Errors
    /// Trace replay failure.
    pub fn from_trace(trace: &Trace) -> anyhow::Result<Self> {
        let (frames, _) = replay(trace, StubColors::default())?;
        let times = trace
            .events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Output))
            .map(|event| event.time_s);
        let frames = frames
            .iter()
            .zip(times)
            .map(|(frame, time_s)| TextFrame {
                time_s,
                rows: selectable_rows(frame),
            })
            .collect();
        Ok(Self {
            plain_text: plain_output_text(trace),
            frames,
        })
    }
}

fn selectable_rows(frame: &Frame) -> Vec<String> {
    let mut rows: Vec<String> = (0..frame.rows())
        .filter_map(|row| frame.row_text(row))
        .collect();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

fn plain_output_text(trace: &Trace) -> String {
    let mut text = String::new();
    for event in &trace.events {
        if matches!(event.kind, EventKind::Output) {
            push_ansi_stripped(&event.data, &mut text);
        }
    }
    text
}

fn push_ansi_stripped(input: &str, out: &mut String) {
    #[derive(Clone, Copy)]
    enum State {
        Ground,
        Esc,
        Csi,
        Osc,
        OscEsc,
        String,
        StringEsc,
    }

    let mut state = State::Ground;
    let mut last_was_cr = false;
    for ch in input.chars() {
        match state {
            State::Ground => match ch {
                '\u{1b}' => {
                    last_was_cr = false;
                    state = State::Esc;
                }
                '\n' => {
                    if !last_was_cr {
                        out.push('\n');
                    }
                    last_was_cr = false;
                }
                '\t' => {
                    last_was_cr = false;
                    out.push(ch);
                }
                '\r' => {
                    out.push('\n');
                    last_was_cr = true;
                }
                ch if !ch.is_control() => {
                    last_was_cr = false;
                    out.push(ch);
                }
                _ => {}
            },
            State::Esc => match ch {
                '[' => state = State::Csi,
                ']' => state = State::Osc,
                'P' | '^' | '_' | 'X' => state = State::String,
                _ => state = State::Ground,
            },
            State::Csi => {
                if ('@'..='~').contains(&ch) {
                    state = State::Ground;
                }
            }
            State::Osc => match ch {
                '\u{7}' => state = State::Ground,
                '\u{1b}' => state = State::OscEsc,
                _ => {}
            },
            State::OscEsc => {
                state = if ch == '\\' {
                    State::Ground
                } else {
                    State::Osc
                };
            }
            State::String => {
                if ch == '\u{1b}' {
                    state = State::StringEsc;
                }
            }
            State::StringEsc => {
                state = if ch == '\\' {
                    State::Ground
                } else {
                    State::String
                };
            }
        }
    }
}

fn media_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("gif") => "image/gif",
        Some("mp4") => "video/mp4",
        _ => "application/octet-stream",
    }
}

fn parse_trace_bytes(bytes: &[u8]) -> anyhow::Result<Trace> {
    let text = std::str::from_utf8(bytes)?;
    Trace::parse(text)
}

fn verify_embedded_hash(label: &str, bytes: &[u8], expected: &str) -> anyhow::Result<()> {
    let actual = sha256_hex(bytes);
    if actual != expected {
        anyhow::bail!("{label} hash mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn ensure_supported_media_type(media_type: &str) -> anyhow::Result<()> {
    if media_type != "video/mp4" {
        anyhow::bail!("ptyrecord v1 embeds browser-controllable MP4 media; got {media_type}");
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("artifact")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::{PtyRecord, plain_output_text};
    use crate::trace::{EventKind, Trace, TraceEvent, TraceHeader};

    fn tiny_trace() -> Trace {
        Trace {
            header: TraceHeader {
                version: 2,
                width: 20,
                height: 4,
                env: std::collections::BTreeMap::default(),
            },
            events: vec![
                TraceEvent {
                    time_s: 0.0,
                    kind: EventKind::Output,
                    data: "\u{1b}[31mhello\u{1b}[0m".into(),
                },
                TraceEvent {
                    time_s: 0.5,
                    kind: EventKind::Output,
                    data: "\r\nworld".into(),
                },
            ],
        }
    }

    #[test]
    fn plain_text_strips_ansi_sequences() {
        assert_eq!(plain_output_text(&tiny_trace()), "hello\nworld");
    }

    #[test]
    fn ptyrecord_embeds_trace_media_and_text() {
        let tmp = TempDir::new().unwrap();
        let trace_path = tmp.path().join("demo.ptytrace");
        let media_path = tmp.path().join("demo.mp4");
        tiny_trace().write(&trace_path).unwrap();
        std::fs::write(&media_path, b"fake media").unwrap();

        let record = PtyRecord::from_paths(&trace_path, &media_path, None).unwrap();

        assert_eq!(record.version, 1);
        assert_eq!(record.trace.path, "demo.ptytrace");
        assert_eq!(record.media.media_type, "video/mp4");
        assert_eq!(record.media.decode().unwrap(), b"fake media");
        assert_eq!(record.transcript.plain_text, "hello\nworld");
        assert!(
            record
                .transcript
                .frames
                .last()
                .unwrap()
                .rows
                .iter()
                .any(|row| row.contains("world"))
        );
    }

    #[test]
    fn read_rejects_wrong_version() {
        let (tmp, mut json) = tiny_record_json();
        let path = tmp.path().join("bad.ptyrecord");
        json["version"] = serde_json::json!(999);
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let err = PtyRecord::read(&path).unwrap_err().to_string();
        assert!(err.contains("unsupported ptyrecord version"));
    }

    #[test]
    fn read_rejects_trace_hash_mismatch() {
        let (tmp, mut json) = tiny_record_json();
        let path = tmp.path().join("bad-trace-hash.ptyrecord");
        json["trace"]["sha256"] = serde_json::json!("00");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let err = PtyRecord::read(&path).unwrap_err().to_string();
        assert!(err.contains("trace hash mismatch"));
    }

    #[test]
    fn read_rejects_media_hash_mismatch() {
        let (tmp, mut json) = tiny_record_json();
        let path = tmp.path().join("bad-media-hash.ptyrecord");
        json["media"]["sha256"] = serde_json::json!("00");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let err = PtyRecord::read(&path).unwrap_err().to_string();
        assert!(err.contains("media hash mismatch"));
    }

    #[test]
    fn read_rejects_stale_transcript_projection() {
        let (tmp, mut json) = tiny_record_json();
        let path = tmp.path().join("bad-transcript.ptyrecord");
        json["transcript"]["plain_text"] = serde_json::json!("not from trace");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let err = PtyRecord::read(&path).unwrap_err().to_string();
        assert!(err.contains("transcript projection does not match embedded trace"));
    }

    fn tiny_record_json() -> (TempDir, serde_json::Value) {
        let tmp = TempDir::new().unwrap();
        let trace_path = tmp.path().join("demo.ptytrace");
        let media_path = tmp.path().join("demo.mp4");
        tiny_trace().write(&trace_path).unwrap();
        std::fs::write(&media_path, b"fake media").unwrap();
        let record = PtyRecord::from_paths(&trace_path, &media_path, None).unwrap();
        (tmp, serde_json::to_value(record).unwrap())
    }

    #[test]
    fn media_type_defaults_for_unknown_extension() {
        assert_eq!(
            super::media_type_for(Path::new("x.bin")),
            "application/octet-stream"
        );
    }

    #[test]
    fn from_paths_rejects_non_mp4_media() {
        let tmp = TempDir::new().unwrap();
        let trace_path = tmp.path().join("demo.ptytrace");
        let media_path = tmp.path().join("demo.gif");
        tiny_trace().write(&trace_path).unwrap();
        std::fs::write(&media_path, b"fake gif").unwrap();

        let err = PtyRecord::from_paths(&trace_path, &media_path, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("MP4 media"));
    }
}
