//! `.ptyrecord` bundle format.
//!
//! A ptyrecord is a self-contained JSON artifact that embeds:
//! - the raw `.ptytrace` bytes,
//! - the rendered media bytes,
//! - optional witness metadata,
//! - selectable text projections derived from replaying the trace.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ptyrender::encode::TimingEntry;
use ptyrender::frame::Frame;
use ptyrender::frame_replay::{ReplayState, TAIL_DWELL_MS, replay};
use ptyrender::paint::{FONT_BYTES, PaintConfig, Painter};
use ptyrender::witness::{Witness, sha256_hex};
use ptytrace::pty::{CaptureEvent, CaptureSink, StubColors};
use ptytrace::trace::{EventKind, Trace};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone)]
pub struct LiveStitchConfig {
    pub font_size_px: f32,
    pub padding_px: u32,
}

impl Default for LiveStitchConfig {
    fn default() -> Self {
        Self {
            font_size_px: 14.0,
            padding_px: 12,
        }
    }
}

/// Output of [`LiveFrameStitcher::finish`]. `frames_dir` is the
/// directory the PNG sequence was written into — it does NOT include
/// ownership of any backing `TempDir`. Callers that hand the stitcher
/// a tempdir-relative path are responsible for keeping that tempdir
/// alive until they're done with `frames_dir` (typically: until
/// `encode()` returns). Dropping the tempdir prematurely deletes the
/// PNGs behind the encoder's back.
#[derive(Debug, Clone)]
pub struct StitchedFrames {
    pub frames_dir: PathBuf,
    pub timing: Vec<TimingEntry>,
}

/// Capture sink that paints output frames while live recording is still
/// running.
///
/// It shares [`ReplayState`] with the batch renderer, so the PNG frames
/// and timing it prepares are equivalent to `replay(trace)` followed by
/// `paint`.
pub struct LiveFrameStitcher {
    frames_dir: PathBuf,
    cfg: LiveStitchConfig,
    replay: Option<ReplayState>,
    painter: Option<Painter<'static>>,
    timing: Vec<TimingEntry>,
    next_frame_index: usize,
}

impl LiveFrameStitcher {
    #[must_use]
    pub fn new(frames_dir: impl Into<PathBuf>, cfg: LiveStitchConfig) -> Self {
        Self {
            frames_dir: frames_dir.into(),
            cfg,
            replay: None,
            painter: None,
            timing: Vec::new(),
            next_frame_index: 1,
        }
    }

    /// Finalize the prepared frame set.
    ///
    /// # Errors
    /// The stitcher was never started by the capture loop.
    pub fn finish(mut self) -> anyhow::Result<StitchedFrames> {
        if self.replay.is_none() {
            anyhow::bail!("live stitcher was not started");
        }
        if let Some(last) = self.timing.last_mut() {
            last.dwell_ms = TAIL_DWELL_MS;
        }
        Ok(StitchedFrames {
            frames_dir: self.frames_dir,
            timing: self.timing,
        })
    }
}

impl CaptureSink for LiveFrameStitcher {
    fn start(&mut self, cols: u16, rows: u16) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.frames_dir)?;
        self.replay = Some(ReplayState::new(cols, rows, StubColors::default())?);
        self.painter = Some(Painter::new(
            FONT_BYTES,
            PaintConfig {
                font_size_px: self.cfg.font_size_px,
                padding_px: self.cfg.padding_px,
                cell_w_px: None,
                cell_h_px: None,
            },
        )?);
        Ok(())
    }

    fn output(&mut self, event: &CaptureEvent) -> anyhow::Result<()> {
        let replay = self
            .replay
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("live stitcher output before start"))?;
        let painter = self
            .painter
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("live stitcher missing painter"))?;
        let frame = replay.process_output(&event.output);
        let frame_name = format!("{:04}", self.next_frame_index);
        self.next_frame_index += 1;
        painter.save_png(&frame, self.frames_dir.join(format!("{frame_name}.png")))?;
        self.timing.push(TimingEntry {
            frame: frame_name,
            dwell_ms: event.dwell_ms,
        });
        Ok(())
    }
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
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).with_context(|| format!("read ptyrecord {}", path.display()))?;
        let record: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse ptyrecord {}", path.display()))?;
        record.validate()?;
        Ok(record)
    }

    /// Validate every embedded consistency claim.
    ///
    /// # Errors
    /// Unsupported schema version, invalid base64, hash mismatch,
    /// malformed trace, stale text projection, or witness mismatch.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.version > PTYRECORD_VERSION {
            anyhow::bail!(
                "ptyrecord version {} not supported by this reader (max supported: {PTYRECORD_VERSION}); upgrade ptyrecord or write with --bundle-version {PTYRECORD_VERSION}",
                self.version
            );
        }
        if self.version < 1 {
            anyhow::bail!(
                "unsupported ptyrecord version {} (minimum supported: 1)",
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
    // Only map extensions that `ensure_supported_media_type` actually
    // accepts. Mapping `.gif` to `"image/gif"` would let
    // `from_paths` produce an EmbeddedFile whose media_type the
    // validator then rejects two lines later — the rejection still
    // fires (and is tested), but advertising "image/gif" as a known
    // type implies bundle support that v1 does not give. Fall through
    // to the opaque catch-all instead so the validator's failure
    // message accurately reflects the bundle format.
    match path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
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

    use super::{LiveFrameStitcher, LiveStitchConfig, PtyRecord, plain_output_text};
    use ptyrender::frame_replay::replay;
    use ptyrender::paint::{FONT_BYTES, PaintConfig, Painter};
    use ptyrender::witness::{RenderOptions, WITNESS_VERSION, Witness, sha256_hex};
    use ptytrace::pty::{CaptureEvent, CaptureSink, StubColors};
    use ptytrace::trace::{EventKind, Trace, TraceEvent, TraceHeader};

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
    fn from_paths_rejects_witness_media_hash_mismatch() {
        let tmp = TempDir::new().unwrap();
        let trace_path = tmp.path().join("demo.ptytrace");
        let media_path = tmp.path().join("demo.mp4");
        tiny_trace().write(&trace_path).unwrap();
        std::fs::write(&media_path, b"fake media").unwrap();
        let trace_sha256 = sha256_hex(&std::fs::read(&trace_path).unwrap());
        let witness = fake_witness(&trace_sha256, "not-the-media-hash");

        let err = PtyRecord::from_paths(&trace_path, &media_path, Some(&witness))
            .unwrap_err()
            .to_string();

        assert!(err.contains("witness output hash does not match embedded media"));
    }

    #[test]
    fn live_stitcher_matches_batch_replay_frames() {
        let tmp = TempDir::new().unwrap();
        let live_dir = tmp.path().join("live");
        let batch_dir = tmp.path().join("batch");
        std::fs::create_dir(&batch_dir).unwrap();

        let mut stitcher = LiveFrameStitcher::new(
            &live_dir,
            LiveStitchConfig {
                font_size_px: 14.0,
                padding_px: 12,
            },
        );
        stitcher.start(20, 4).unwrap();
        stitcher
            .output(&CaptureEvent {
                time_s: 0.0,
                output: b"hello".to_vec(),
                dwell_ms: 120,
            })
            .unwrap();
        stitcher
            .output(&CaptureEvent {
                time_s: 0.120,
                output: b" world".to_vec(),
                dwell_ms: 250,
            })
            .unwrap();
        let prepared_frames = stitcher.finish().unwrap();

        let trace = Trace {
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
                    data: "hello".into(),
                },
                TraceEvent {
                    time_s: 0.120,
                    kind: EventKind::Output,
                    data: " world".into(),
                },
            ],
        };
        let (frames, timing) = replay(&trace, StubColors::default()).unwrap();
        let painter = Painter::new(
            FONT_BYTES,
            PaintConfig {
                font_size_px: 14.0,
                padding_px: 12,
                cell_w_px: None,
                cell_h_px: None,
            },
        )
        .unwrap();
        for (frame, entry) in frames.iter().zip(&timing) {
            painter
                .save_png(frame, batch_dir.join(format!("{}.png", entry.frame)))
                .unwrap();
        }

        assert_eq!(prepared_frames.timing.len(), timing.len());
        assert_eq!(prepared_frames.timing[0].dwell_ms, timing[0].dwell_ms);
        assert_eq!(prepared_frames.timing[1].dwell_ms, timing[1].dwell_ms);
        for entry in &timing {
            let live_png = std::fs::read(live_dir.join(format!("{}.png", entry.frame))).unwrap();
            let batch_png = std::fs::read(batch_dir.join(format!("{}.png", entry.frame))).unwrap();
            assert_eq!(sha256_hex(&live_png), sha256_hex(&batch_png));
        }
    }

    #[test]
    fn read_rejects_wrong_version() {
        let (tmp, mut json) = tiny_record_json();
        let path = tmp.path().join("bad.ptyrecord");
        json["version"] = serde_json::json!(999);
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let err = PtyRecord::read(&path).unwrap_err().to_string();
        assert!(err.contains("ptyrecord version 999 not supported by this reader"));
        assert!(err.contains("max supported: 1"));
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

    #[test]
    fn read_io_error_includes_path() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist.ptyrecord");

        let err = PtyRecord::read(&missing).unwrap_err();
        // Top-level message names the operation and the path; the
        // underlying IO error remains accessible via the source chain.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("read ptyrecord"),
            "missing 'read ptyrecord' prefix: {chain}"
        );
        assert!(
            chain.contains(missing.to_str().unwrap()),
            "missing path in error: {chain}"
        );
    }

    #[test]
    fn read_parse_error_includes_path() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bad.ptyrecord");
        std::fs::write(&path, b"not valid json at all").unwrap();

        let err = PtyRecord::read(&path).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("parse ptyrecord"),
            "missing 'parse ptyrecord' prefix: {chain}"
        );
        assert!(
            chain.contains(path.to_str().unwrap()),
            "missing path in error: {chain}"
        );
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

    fn fake_witness(trace_sha256: &str, output_sha256: &str) -> Witness {
        serde_json::from_value(serde_json::json!({
            "version": WITNESS_VERSION,
            "tool": {
                "name": "ptytrace-test",
                "version": "0",
                "ffmpeg_version": "ffmpeg test",
                "font_sha256": "font"
            },
            "trace_sha256": trace_sha256,
            "render": RenderOptions::libx264(14.0, 12, None, 30),
            "output_sha256": output_sha256,
            "output_filename": "demo.mp4"
        }))
        .unwrap()
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
