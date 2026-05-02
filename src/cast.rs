//! asciinema v2 cast file format.
//!
//! A cast is a JSONL file: line 0 is a [`CastHeader`] object, lines 1..N
//! are 3-element arrays `[time_seconds, "o"|"i", data_string]`. The recorder
//! emits casts whose timestamps are the cumulative sum of intent-based
//! `dwell_ms`, never wall-clock — this is what makes playback deterministic.
//!
//! Spec: <https://docs.asciinema.org/manual/asciicast/v2/>

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastHeader {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// Subset of the env namespace baked into the cast for parity with
    /// asciinema's reference player. We only emit `TERM` and `SHELL`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Output,
    Input,
}

impl EventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::Output => "o",
            EventKind::Input => "i",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CastEvent {
    pub time_s: f64,
    pub kind: EventKind,
    pub data: String,
}

impl Serialize for CastEvent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(3))?;
        seq.serialize_element(&self.time_s)?;
        seq.serialize_element(self.kind.as_str())?;
        seq.serialize_element(&self.data)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for CastEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // 3-element heterogeneous array. Use a tuple deserialize with the
        // kind field as a single-char string.
        let (time_s, kind_str, data): (f64, String, String) =
            Deserialize::deserialize(d)?;
        let kind = match kind_str.as_str() {
            "o" => EventKind::Output,
            "i" => EventKind::Input,
            other => return Err(serde::de::Error::custom(
                format!("unknown cast event kind: {other:?}"),
            )),
        };
        Ok(CastEvent { time_s, kind, data })
    }
}

/// In-memory cast: header + events, deterministic order.
#[derive(Debug, Clone)]
pub struct Cast {
    pub header: CastHeader,
    pub events: Vec<CastEvent>,
}

impl Cast {
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut lines = text.lines().filter(|l| !l.is_empty());
        let header_line = lines.next().ok_or_else(|| anyhow::anyhow!("empty cast"))?;
        let header: CastHeader = serde_json::from_str(header_line)?;
        let events = lines
            .map(serde_json::from_str)
            .collect::<Result<Vec<CastEvent>, _>>()?;
        Ok(Cast { header, events })
    }

    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        std::fs::write(path.as_ref(), self.to_string())?;
        Ok(())
    }

    pub fn to_string(&self) -> String {
        let mut out = serde_json::to_string(&self.header).expect("header serializable");
        for ev in &self.events {
            out.push('\n');
            out.push_str(&serde_json::to_string(ev).expect("event serializable"));
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = CastHeader {
            version: 2,
            width: 80,
            height: 30,
            env: [("TERM".into(), "xterm-256color".into())].into_iter().collect(),
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: CastHeader = serde_json::from_str(&s).unwrap();
        assert_eq!(back.width, 80);
        assert_eq!(back.height, 30);
        assert_eq!(back.env.get("TERM").unwrap(), "xterm-256color");
    }

    #[test]
    fn event_serializes_as_3_array() {
        let ev = CastEvent {
            time_s: 1.234,
            kind: EventKind::Output,
            data: "hi".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert_eq!(s, r#"[1.234,"o","hi"]"#);
    }

    #[test]
    fn event_deserializes_from_3_array() {
        let ev: CastEvent = serde_json::from_str(r#"[2.5,"i","x"]"#).unwrap();
        assert_eq!(ev.time_s, 2.5);
        assert_eq!(ev.kind, EventKind::Input);
        assert_eq!(ev.data, "x");
    }

    #[test]
    fn rejects_unknown_event_kind() {
        let r: Result<CastEvent, _> = serde_json::from_str(r#"[0.0,"z","x"]"#);
        assert!(r.is_err());
    }

    #[test]
    fn cast_round_trip() {
        let c = Cast {
            header: CastHeader { version: 2, width: 80, height: 30, env: Default::default() },
            events: vec![
                CastEvent { time_s: 0.0,  kind: EventKind::Output, data: "hello".into() },
                CastEvent { time_s: 0.5,  kind: EventKind::Output, data: " world".into() },
            ],
        };
        let s = c.to_string();
        let back = Cast::parse(&s).unwrap();
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.events[1].data, " world");
    }
}
