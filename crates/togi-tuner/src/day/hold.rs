use crate::UnitProfile;
use togi_journal::Seq;

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
        profile: UnitProfile,
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

/// Credited seconds and the indices of their interval witnesses at exactly `profile`.
///
/// Only verified applied intervals recorded after `origin` count (`docs/spec/tuner.md`, Hold and
/// next proposal). Witness indices preserve input order and let callers recover commitment identities
/// without allocating when only seconds are needed. Suspended, paused, unobserved, ambiguous and
/// other-profile time is excluded.
pub fn hold_accounting<'a>(
    intervals: &'a [DayInterval],
    profile: &'a UnitProfile,
    origin: Seq,
) -> (u64, impl Iterator<Item = usize> + 'a) {
    let credited = intervals.iter().enumerate().filter(move |(_, interval)| {
        interval.at > origin
            && matches!(&interval.kind, IntervalKind::Applied {
                profile: observed,
                readback: Readback::Verified,
            } if observed == profile)
    });
    let seconds = credited
        .clone()
        .map(|(_, interval)| interval.secs)
        .fold(0_u64, saturating_add_secs);
    (seconds, credited.map(|(index, _)| index))
}

/// Credited seconds without materializing interval witnesses.
pub fn credited_secs(intervals: &[DayInterval], profile: &UnitProfile, origin: Seq) -> u64 {
    hold_accounting(intervals, profile, origin).0
}

/// Credited seconds saturate at the largest count a hold can represent.
fn saturating_add_secs(total: u64, secs: u64) -> u64 {
    total.saturating_add(secs)
}
