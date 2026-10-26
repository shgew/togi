//! A CPU-suspect event's one reproduction night (`docs/spec/tuner.md`, Night order and reserved
//! budgets): exact profile first, then each CCD alone, each attempt stopping at its first conclusive
//! failure.

use std::collections::HashSet;

use togi_journal::Seq;

use super::matrix::{Attempt, AttemptScope, Cell};
use super::schedule::{Reason, Reproduction, ReproductionStage, Work};

/// What the event's reproduction trials have shown so far.
#[derive(Clone, Debug, Default)]
pub struct AttemptLog {
    /// Cells that ran conclusively clean, per attempt.
    pub clean: HashSet<(AttemptScope, Cell)>,
    /// Attempts that hit a conclusive failure and stop.
    pub failed: HashSet<AttemptScope>,
}

/// Whose night the attempt is running on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptNight {
    /// The night the event's reproduction began, or the one now starting it.
    This,
    /// A night that already ended, including one a crash interrupted.
    Earlier,
}

/// The stage of `event`'s reproduction. The first attempt that did not fail and still has cells
/// outstanding is next. Outstanding cells on an earlier night lapse: the event gets one night only
/// and ends unreproduced.
#[must_use]
pub fn reproduction_stage(
    event: Seq,
    attempts: &[Attempt],
    log: &AttemptLog,
    night: AttemptNight,
) -> Reproduction {
    let open = attempts
        .iter()
        .filter(|attempt| !log.failed.contains(&attempt.scope))
        .find_map(|attempt| {
            let cells: Vec<Cell> = attempt
                .matrix
                .fresh()
                .filter(|cell| !log.clean.contains(&(attempt.scope, **cell)))
                .copied()
                .collect();
            (!cells.is_empty()).then(|| Work {
                profile: attempt.matrix.profile.clone(),
                cells,
                reason: Reason::Reproduction {
                    event,
                    attempt: attempt.scope,
                },
            })
        });
    let stage = match (open, night) {
        (Some(work), AttemptNight::This) => ReproductionStage::Open(work),
        (Some(_), AttemptNight::Earlier) => ReproductionStage::Lapsed,
        (None, AttemptNight::This | AttemptNight::Earlier) => ReproductionStage::Concluded,
    };
    Reproduction { event, stage }
}
