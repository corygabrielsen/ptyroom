//! Semantic trace + presentation timeline compiler.
//!
//! The trace records causal facts from a live terminal run: bytes
//! observed, gates crossed, and named presentation beats. The compiler
//! turns those beats into asciinema timestamps using a marketing policy.
//! Wall-clock capture latency is diagnostic only; it never becomes
//! playback time.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cast::{Cast, CastEvent, CastHeader, EventKind};

/// Named viewer-facing moments the marketing timing policy can tune.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationBeat {
    TypeChar,
    PickerAppeared,
    PickerNav,
    PickerOvershoot,
    PickerSelected,
    PickerDigest,
}

/// Typed policy for converting semantic beats to presentation time.
#[derive(Debug, Clone)]
pub struct TimelinePolicy {
    pub type_char: Duration,
    pub picker_appeared: Duration,
    pub picker_nav: Duration,
    pub picker_overshoot: Duration,
    pub picker_selected: Duration,
    pub picker_digest: Duration,
}

impl Default for TimelinePolicy {
    fn default() -> Self {
        Self {
            type_char: Duration::from_millis(24),
            picker_appeared: Duration::from_millis(500),
            picker_nav: Duration::from_millis(50),
            picker_overshoot: Duration::from_millis(500),
            picker_selected: Duration::from_secs(1),
            picker_digest: Duration::from_secs(2),
        }
    }
}

impl TimelinePolicy {
    #[must_use]
    pub fn dwell_for(&self, beat: PresentationBeat) -> Duration {
        match beat {
            PresentationBeat::TypeChar => self.type_char,
            PresentationBeat::PickerAppeared => self.picker_appeared,
            PresentationBeat::PickerNav => self.picker_nav,
            PresentationBeat::PickerOvershoot => self.picker_overshoot,
            PresentationBeat::PickerSelected => self.picker_selected,
            PresentationBeat::PickerDigest => self.picker_digest,
        }
    }
}

/// One causal trace entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceEvent {
    /// Terminal bytes observed at a causal boundary. `beat` controls how
    /// long the resulting state should be presented after replay.
    Output {
        #[serde(with = "hex_bytes")]
        bytes: Vec<u8>,
        beat: Option<PresentationBeat>,
    },
    /// A pure presentation beat with no associated terminal bytes.
    Beat { beat: PresentationBeat },
    /// Diagnostic marker: a causal gate fired after `elapsed_ms`.
    Marker { name: String, elapsed_ms: u64 },
}

/// Causal terminal trace, independent of presentation timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub cols: u16,
    pub rows: u16,
    pub events: Vec<TraceEvent>,
}

impl Trace {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            events: Vec::new(),
        }
    }

    pub fn push_output(&mut self, bytes: Vec<u8>, beat: Option<PresentationBeat>) {
        self.events.push(TraceEvent::Output { bytes, beat });
    }

    pub fn push_beat(&mut self, beat: PresentationBeat) {
        self.events.push(TraceEvent::Beat { beat });
    }

    pub fn push_marker(&mut self, name: impl Into<String>, elapsed: Duration) {
        self.events.push(TraceEvent::Marker {
            name: name.into(),
            elapsed_ms: duration_to_ms(elapsed),
        });
    }

    /// Write this trace as pretty JSON for inspection and future tests.
    ///
    /// # Errors
    /// IO or JSON serialization error.
    pub fn write_json(&self, path: impl AsRef<std::path::Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    #[must_use]
    pub fn compile_cast(&self, policy: &TimelinePolicy) -> Cast {
        let header = CastHeader {
            version: 2,
            width: u32::from(self.cols),
            height: u32::from(self.rows),
            env: [
                ("TERM".into(), "xterm-256color".into()),
                ("SHELL".into(), "/bin/bash".into()),
            ]
            .into_iter()
            .collect(),
        };

        let mut events = Vec::new();
        let mut t_ms: u64 = 0;
        let mut last_output_t_ms: u64 = 0;
        for event in &self.events {
            match event {
                TraceEvent::Output { bytes, beat } => {
                    if !bytes.is_empty() {
                        events.push(CastEvent {
                            time_s: ms_to_seconds(t_ms),
                            kind: EventKind::Output,
                            data: String::from_utf8_lossy(bytes).into_owned(),
                        });
                        last_output_t_ms = t_ms;
                    }
                    if let Some(beat) = beat {
                        t_ms = t_ms.saturating_add(duration_to_ms(policy.dwell_for(*beat)));
                    }
                }
                TraceEvent::Beat { beat } => {
                    t_ms = t_ms.saturating_add(duration_to_ms(policy.dwell_for(*beat)));
                }
                TraceEvent::Marker { .. } => {}
            }
        }

        if t_ms > last_output_t_ms && !events.is_empty() {
            events.push(CastEvent {
                time_s: ms_to_seconds(t_ms),
                kind: EventKind::Output,
                data: String::new(),
            });
        }

        Cast { header, events }
    }
}

fn duration_to_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[allow(clippy::cast_precision_loss)]
fn ms_to_seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut out, "{byte:02x}").expect("infallible String fmt");
        }
        serializer.serialize_str(&out)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.len() % 2 != 0 {
            return Err(serde::de::Error::custom("hex byte string has odd length"));
        }

        let mut bytes = Vec::with_capacity(s.len() / 2);
        for chunk in s.as_bytes().chunks_exact(2) {
            let high = decode_nibble::<D::Error>(chunk[0])?;
            let low = decode_nibble::<D::Error>(chunk[1])?;
            bytes.push((high << 4) | low);
        }
        Ok(bytes)
    }

    fn decode_nibble<E>(byte: u8) -> Result<u8, E>
    where
        E: serde::de::Error,
    {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(E::custom("hex byte string contains non-hex digit")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_cast_uses_policy_not_marker_latency() {
        let mut trace = Trace::new(80, 20);
        trace.push_marker("slow_machine", Duration::from_secs(30));
        trace.push_output(b"a".to_vec(), Some(PresentationBeat::TypeChar));
        trace.push_output(b"b".to_vec(), Some(PresentationBeat::PickerNav));

        let policy = TimelinePolicy {
            type_char: Duration::from_millis(10),
            picker_nav: Duration::from_millis(20),
            ..TimelinePolicy::default()
        };
        let cast = trace.compile_cast(&policy);
        assert!(cast.events[0].time_s.abs() < 1e-9);
        assert!((cast.events[1].time_s - 0.010).abs() < 1e-9);
        assert!((cast.events[2].time_s - 0.030).abs() < 1e-9);
    }

    #[test]
    fn pure_beat_advances_next_output() {
        let mut trace = Trace::new(80, 20);
        trace.push_beat(PresentationBeat::PickerOvershoot);
        trace.push_output(b"x".to_vec(), None);
        let cast = trace.compile_cast(&TimelinePolicy::default());
        assert!((cast.events[0].time_s - 0.5).abs() < 1e-9);
    }

    #[test]
    fn json_serializes_output_bytes_as_hex() {
        let mut trace = Trace::new(80, 20);
        trace.push_output(b"\x1b[A".to_vec(), Some(PresentationBeat::PickerNav));

        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains(r#""bytes":"1b5b41""#));

        let back: Trace = serde_json::from_str(&json).unwrap();
        match &back.events[0] {
            TraceEvent::Output { bytes, beat } => {
                assert_eq!(bytes, b"\x1b[A");
                assert_eq!(*beat, Some(PresentationBeat::PickerNav));
            }
            other => panic!("expected output event, got {other:?}"),
        }
    }
}
