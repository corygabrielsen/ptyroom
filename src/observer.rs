//! Deterministic observers and semantic predicates.
//!
//! An observer is the smallest state machine that can replay raw output bytes
//! and answer predicates about the resulting state. This module starts with a
//! synthetic text observer so the proof pipeline can be tested without binding
//! the architecture to any specific terminal implementation.

use serde::{Deserialize, Serialize};

use crate::proof::StateHash;

/// Semantic fact extracted from an observed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Fact {
    TextBytes { count: usize },
    EventCount { count: u64 },
    StateHash { hash: StateHash },
}

/// Snapshot of an observer state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedState {
    text: String,
    event_count: u64,
    hash: StateHash,
}

impl ObservedState {
    #[must_use]
    pub fn new(text: String, event_count: u64) -> Self {
        let hash = stable_state_hash(text.as_bytes(), event_count);
        Self {
            text,
            event_count,
            hash,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    #[must_use]
    pub const fn hash(&self) -> StateHash {
        self.hash
    }

    #[must_use]
    pub fn facts(&self) -> Vec<Fact> {
        vec![
            Fact::TextBytes {
                count: self.text.len(),
            },
            Fact::EventCount {
                count: self.event_count,
            },
            Fact::StateHash { hash: self.hash },
        ]
    }
}

/// Predicate that can be checked against an [`ObservedState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Predicate {
    ContainsText { text: String },
    DoesNotContainText { text: String },
    StateEquals { hash: StateHash },
    EventCountIs { count: u64 },
}

impl Predicate {
    #[must_use]
    pub fn matches(&self, state: &ObservedState) -> bool {
        match self {
            Self::ContainsText { text } => state.text.contains(text),
            Self::DoesNotContainText { text } => !state.text.contains(text),
            Self::StateEquals { hash } => state.hash == *hash,
            Self::EventCountIs { count } => state.event_count == *count,
        }
    }
}

/// Deterministic state machine over output bytes.
pub trait Observer {
    fn apply_output(&mut self, bytes: &[u8]);
    fn state(&self) -> ObservedState;

    #[must_use]
    fn satisfies(&self, predicate: &Predicate) -> bool {
        predicate.matches(&self.state())
    }
}

/// Minimal observer for architecture tests and synthetic fixtures.
#[derive(Debug, Clone, Default)]
pub struct SyntheticObserver {
    text: String,
    event_count: u64,
}

impl SyntheticObserver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Observer for SyntheticObserver {
    fn apply_output(&mut self, bytes: &[u8]) {
        self.text.push_str(&String::from_utf8_lossy(bytes));
        self.event_count = self.event_count.saturating_add(1);
    }

    fn state(&self) -> ObservedState {
        ObservedState::new(self.text.clone(), self.event_count)
    }
}

#[must_use]
fn stable_state_hash(text: &[u8], event_count: u64) -> StateHash {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.iter().chain(event_count.to_le_bytes().iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    StateHash::new(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_observer_accumulates_lossy_text() {
        let mut observer = SyntheticObserver::new();
        observer.apply_output(b"hello ");
        observer.apply_output(&[b'w', b'o', b'r', b'l', b'd', 0xff]);

        let state = observer.state();
        assert!(state.text().contains("hello world"));
        assert_eq!(state.event_count(), 2);
    }

    #[test]
    fn predicates_match_observed_state() {
        let state = ObservedState::new("alpha beta".into(), 3);
        assert!(
            Predicate::ContainsText {
                text: "beta".into()
            }
            .matches(&state)
        );
        assert!(
            Predicate::DoesNotContainText {
                text: "gamma".into()
            }
            .matches(&state)
        );
        assert!(Predicate::EventCountIs { count: 3 }.matches(&state));
        assert!(Predicate::StateEquals { hash: state.hash() }.matches(&state));
    }

    #[test]
    fn state_hash_is_stable_and_state_sensitive() {
        let a = ObservedState::new("same".into(), 1).hash();
        let b = ObservedState::new("same".into(), 1).hash();
        let c = ObservedState::new("same".into(), 2).hash();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn facts_expose_state_summary() {
        let state = ObservedState::new("abc".into(), 2);
        assert_eq!(
            state.facts(),
            vec![
                Fact::TextBytes { count: 3 },
                Fact::EventCount { count: 2 },
                Fact::StateHash { hash: state.hash() },
            ]
        );
    }
}
