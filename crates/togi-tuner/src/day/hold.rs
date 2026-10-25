use togi_journal::Seq;
use togi_machine::CcdProfile;

/// Whether an applied interval's readback was verified unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readback {
    /// The checkpointed readback matched the profile through the whole interval.
    Verified,
    /// Any other readback: a changed or missing one is no evidence of the profile.
    Unverified,
}

/// What an interval of the day was (`docs/spec/journal.md`, `day.interval`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IntervalKind {
    /// Observed active at `profile`.
    Applied {
        profile: CcdProfile,
        readback: Readback,
    },
    Paused,
    Suspended,
    Unobserved,
    /// Ended in a changed offset: credited to no profile.
    Ambiguous,
}

/// A typed interval of the day from a checkpointed observer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayInterval {
    /// The journal position of the interval's record.
    pub at: Seq,
    /// Whole seconds of the interval.
    pub secs: u64,
    pub kind: IntervalKind,
}

/// The seconds that count toward a hold at `profile`: applied intervals with a verified unchanged
/// readback at exactly that profile, recorded after `origin` (`docs/spec/tuner.md`, Hold and next
/// proposal). Suspended, paused, unobserved, ambiguous and other-profile time is excluded.
pub fn credited_secs(intervals: &[DayInterval], profile: &CcdProfile, origin: Seq) -> u64 {
    intervals
        .iter()
        .filter(|interval| interval.at > origin)
        .filter_map(|interval| match &interval.kind {
            IntervalKind::Applied {
                profile: observed,
                readback: Readback::Verified,
            } if observed == profile => Some(interval.secs),
            IntervalKind::Applied { .. }
            | IntervalKind::Paused
            | IntervalKind::Suspended
            | IntervalKind::Unobserved
            | IntervalKind::Ambiguous => None,
        })
        .fold(0_u64, saturating_add_secs)
}

/// Credited seconds saturate at the largest count a hold can represent.
fn saturating_add_secs(total: u64, secs: u64) -> u64 {
    total.saturating_add(secs)
}
