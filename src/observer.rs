//! Predicates checked against captured output during recording.
//!
//! A [`Predicate`] is an optional content-shaped assertion attached
//! to a step in [`crate::recording::RecordingBuilder`]. When the
//! recorder is set up to verify a step (`record_step_matching`), the
//! predicate runs at record time against the accumulated UTF-8-lossy
//! text of all output bytes seen so far. A predicate that fails halts
//! recording with an error — caller decides whether to retry, abort,
//! or extend the recording's settle window.

use serde::{Deserialize, Serialize};

/// Content-shaped assertion checked against captured terminal output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum Predicate {
    /// `text` must appear in the captured output text.
    ContainsText { text: String },
    /// `text` must NOT appear in the captured output text.
    DoesNotContainText { text: String },
}

impl Predicate {
    /// Evaluate the predicate against `output_text`.
    #[must_use]
    pub fn check(&self, output_text: &str) -> bool {
        match self {
            Self::ContainsText { text } => output_text.contains(text),
            Self::DoesNotContainText { text } => !output_text.contains(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_text_matches_substring() {
        let p = Predicate::ContainsText {
            text: "hello".into(),
        };
        assert!(p.check("well hello there"));
        assert!(!p.check("goodbye"));
    }

    #[test]
    fn does_not_contain_text_inverse() {
        let p = Predicate::DoesNotContainText {
            text: "error".into(),
        };
        assert!(p.check("all good"));
        assert!(!p.check("fatal error: foo"));
    }
}
