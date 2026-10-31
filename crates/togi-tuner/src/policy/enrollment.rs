//! Whether a daily event's crash precedes the machine's first campaign enrollment.
//!
//! A crash before the first `campaign.start` happened outside the campaign: it vetoes its whole
//! profile and everything at least as deep, and nothing else. It is no learning event, stops no
//! unit and needs no applied-step ancestry. The boundary is that first start, original or fresh,
//! never a successor's restart. The rule is shared by the live fold and the predecessor's recovery,
//! which read different records of the same facts.

use jiff::Timestamp;
use togi_machine::BootId;

/// Where the machine's first campaign enrollment was recorded.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Enrollment {
    pub(super) time: Timestamp,
    pub(super) boot: BootId,
}

/// What is known of when one crash identity happened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CrashTime {
    /// A crash no observer saw, at the instant its record named.
    At(Timestamp),
    /// A named boot. A crash cannot be later than the first record that observed it, however late
    /// a copy of it is written; and a boot other than the enrolling one that left no record at or
    /// after the enrollment ended before it.
    Boot {
        boot: BootId,
        /// The earliest record that observed the boot's crash or machine checks.
        observed: Option<Timestamp>,
        /// The latest record of the boot's own observer, original or imported.
        last: Option<Timestamp>,
    },
    /// An identity with no recoverable timeline, such as a failed re-qualification: it can only
    /// occur under a campaign, so it stays inside it.
    Unknown,
}

/// Whether `crash` happened strictly before the machine's first enrollment. A crash at the
/// enrollment or later, and any crash whose time is unknown, keeps the full campaign behavior.
///
/// Without an `enrollment` the predecessor's history is read before its successor enrolls, and
/// every imported record is older than that enrollment: any crash with a timeline precedes it.
pub(super) fn precedes(enrollment: Option<&Enrollment>, crash: &CrashTime) -> bool {
    match (enrollment, crash) {
        (Some(enrollment), CrashTime::At(at)) => *at < enrollment.time,
        (None, CrashTime::At(_)) => true,
        (
            Some(enrollment),
            CrashTime::Boot {
                boot,
                observed,
                last,
            },
        ) => {
            observed.is_some_and(|at| at < enrollment.time)
                || (*boot != enrollment.boot && last.is_some_and(|at| at < enrollment.time))
        }
        (None, CrashTime::Boot { observed, last, .. }) => observed.is_some() || last.is_some(),
        (_, CrashTime::Unknown) => false,
    }
}
