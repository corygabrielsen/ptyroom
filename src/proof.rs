//! Proof-state markers and small invariant-carrying scalar types.
//!
//! The marker types are intentionally zero-sized. They let public data
//! structures expose only the transitions that are valid for their current
//! lifecycle stage, while keeping constructors private to each owning module.

use std::time::Duration;

use serde::{Deserialize, Serialize};

mod sealed {
    pub trait Sealed {}
}

/// Marker trait for typestate parameters used by the proof pipeline.
pub trait ProofState: sealed::Sealed + Copy + std::fmt::Debug + 'static {}

/// Raw evidence is still being collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Open;
/// Raw evidence has been closed and can no longer be appended to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;
/// Semantic transitions exist but predicates have not yet been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unverified;
/// Semantic transitions have been replay-verified against their predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified;
/// Timeline timestamps are known to be monotonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monotonic;

impl sealed::Sealed for Open {}
impl sealed::Sealed for Closed {}
impl sealed::Sealed for Unverified {}
impl sealed::Sealed for Verified {}
impl sealed::Sealed for Monotonic {}

impl ProofState for Open {}
impl ProofState for Closed {}
impl ProofState for Unverified {}
impl ProofState for Verified {}
impl ProofState for Monotonic {}

/// Monotonic raw event sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(u64);

impl Seq {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable identifier for an intended action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntentId(u64);

impl IntentId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable hash of an observer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StateHash(u64);

impl StateHash {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Presentation dwell in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DwellMs(u64);

impl DwellMs {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        Self(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn checked_add(self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Presentation timestamp in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimestampMs(u64);

impl TimestampMs {
    #[must_use]
    pub const fn zero() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn checked_add(self, dwell: DwellMs) -> Option<Self> {
        match self.0.checked_add(dwell.0) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_checked_add_detects_overflow() {
        let ts = TimestampMs(u64::MAX);
        assert!(ts.checked_add(DwellMs::new(1)).is_none());
    }

    #[test]
    fn scalar_accessors_round_trip() {
        assert_eq!(Seq::new(7).get(), 7);
        assert_eq!(IntentId::new(8).get(), 8);
        assert_eq!(StateHash::new(9).get(), 9);
        assert_eq!(DwellMs::new(10).get(), 10);
    }

    #[test]
    fn dwell_from_duration_saturates_to_milliseconds() {
        assert_eq!(DwellMs::from_duration(Duration::from_millis(12)).get(), 12);
    }

    #[test]
    fn dwell_checked_add_detects_overflow() {
        assert!(
            DwellMs::new(u64::MAX)
                .checked_add(DwellMs::new(1))
                .is_none()
        );
    }
}
