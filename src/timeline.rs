//! Presentation timing policy.
//!
//! The proof-backed recorder owns raw evidence, semantic verification, and
//! monotonic cast compilation. This module is intentionally smaller: it names
//! viewer-facing beats and maps them to deterministic presentation dwell.

use std::time::Duration;

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_maps_named_beats_to_dwell() {
        let policy = TimelinePolicy {
            type_char: Duration::from_millis(10),
            picker_nav: Duration::from_millis(20),
            ..TimelinePolicy::default()
        };

        assert_eq!(
            policy.dwell_for(PresentationBeat::TypeChar),
            Duration::from_millis(10)
        );
        assert_eq!(
            policy.dwell_for(PresentationBeat::PickerNav),
            Duration::from_millis(20)
        );
    }

    #[test]
    fn default_picker_digest_is_slowest_beat() {
        let policy = TimelinePolicy::default();

        assert!(policy.dwell_for(PresentationBeat::PickerDigest) > policy.picker_selected);
    }
}
