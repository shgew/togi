use togi_machine::TargetOffset;

use crate::unit::{UnitId, UnitProfile};

use super::{Ceilings, FinishBook};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub unit: UnitId,
    pub from: TargetOffset,
    pub to: TargetOffset,
    pub remaining: i64,
    pub step: i64,
    pub profile: UnitProfile,
}

/// Select once before checking qualification: a blocked vector cannot be evaded.
pub fn propose(
    applied: &UnitProfile,
    ceilings: &Ceilings,
    finishes: &FinishBook,
) -> Option<Proposal> {
    let mut best = None;
    for (unit, from) in applied.iter() {
        if finishes.is_finished(unit) {
            continue;
        }
        let Some(c) = ceilings.get(&unit).and_then(|ceiling| ceiling.c) else {
            continue;
        };
        let remaining = from.get() - c.get();
        if remaining <= 0 {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|proposal: &Proposal| remaining > proposal.remaining)
        {
            let step = ((remaining + 1) / 2).clamp(1, 5);
            let to = TargetOffset::clamped(from.get() - step);
            best = Some(Proposal {
                unit,
                from,
                to,
                remaining,
                step,
                profile: applied.with(unit, to),
            });
        }
    }
    best
}
