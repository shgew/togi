use std::collections::BTreeSet;

use togi_machine::TargetOffset;

use crate::day::{DailyEvent, EventId, Veto};
use crate::unit::{UnitId, UnitProfile};

use super::{Ceilings, ClimbError, ClimbHistory, Cursor};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Retreat {
    /// Exact qualified safety target, or stock while `wanted` is unqualified.
    pub profile: UnitProfile,
    pub wanted: UnitProfile,
    pub events: Vec<EventId>,
    pub cursors: Vec<Cursor>,
    pub stopped: Vec<UnitId>,
    pub adverse_units: Vec<UnitId>,
}

pub(super) fn forbidden(
    history: &ClimbHistory,
    profile: &UnitProfile,
    vetoes: &[Veto],
) -> Result<bool, ClimbError> {
    let actual = history.actual(profile)?;
    Ok(vetoes.iter().any(|veto| veto.profile.forbids(&actual)))
}

fn backstep(
    history: &ClimbHistory,
    current: &UnitProfile,
    consumed: &mut BTreeSet<Cursor>,
    all: bool,
) -> Result<(UnitProfile, Vec<Cursor>), ClimbError> {
    let choices: Vec<_> = current
        .iter()
        .map(|(unit, now)| history.newest(unit, now, consumed))
        .collect();
    let newest = choices
        .iter()
        .flatten()
        .map(|link| link.transition.application)
        .max();
    let mut cursors = Vec::new();
    let values = current
        .iter()
        .zip(choices)
        .map(|((unit, now), link)| {
            let selected = link.filter(|link| all || Some(link.transition.application) == newest);
            let depth = if let Some(link) = selected {
                let cursor = Cursor {
                    unit,
                    transition: link.transition,
                };
                consumed.insert(cursor);
                cursors.push(cursor);
                now.max(link.before)
            } else if all || newest.is_none() {
                TargetOffset::ZERO
            } else {
                now
            };
            (unit, depth)
        })
        .collect::<Vec<_>>();
    Ok((UnitProfile::try_from(values)?, cursors))
}

/// Compute safety from actual applications and durable cursors, never from plans or current charges.
pub(super) fn plan(
    history: &ClimbHistory,
    events: &[DailyEvent],
    vetoes: &[Veto],
    ceilings: &Ceilings,
) -> Result<Option<Retreat>, ClimbError> {
    let mut wanted = history.current().clone();
    let mut consumed = history.consumed().clone();
    let mut cursors = Vec::new();
    let mut answered = Vec::new();
    let mut tails = Vec::new();
    for record in history.retreats() {
        if !record.events.is_empty() {
            tails.push(
                super::SavedProfile {
                    units: record.units.clone(),
                    profile: record.after.clone(),
                }
                .actual()?,
            );
        }
    }
    for event in events {
        if !event.class.is_cpu_suspect()
            || history
                .retreats()
                .iter()
                .any(|record| record.events.contains(&event.id))
        {
            continue;
        }
        if !event.profile.forbids(&history.actual(&wanted)?) {
            if !answered.is_empty() {
                answered.push(event.id.clone());
            }
            continue;
        }
        let all = tails.contains(&event.profile);
        let (next, reverted) = backstep(history, &wanted, &mut consumed, all)?;
        tails.push(history.actual(&next)?);
        wanted = next;
        cursors.extend(reverted);
        answered.push(event.id.clone());
    }
    let mut adverse_units = Vec::new();
    wanted = UnitProfile::try_from(
        wanted
            .iter()
            .map(|(unit, depth)| {
                if let Some(c) = ceilings
                    .get(&unit)
                    .and_then(|ceiling| ceiling.c)
                    .filter(|&c| depth < c)
                {
                    adverse_units.push(unit);
                    (unit, c)
                } else {
                    (unit, depth)
                }
            })
            .collect::<Vec<_>>(),
    )?;
    while !wanted.is_zero() && forbidden(history, &wanted, vetoes)? {
        let (next, reverted) = backstep(history, &wanted, &mut consumed, true)?;
        cursors.extend(reverted);
        wanted = if next == wanted {
            UnitProfile::zero(history.units())
        } else {
            next
        };
    }
    if wanted == *history.current() && answered.is_empty() {
        return Ok(None);
    }
    let stopped = wanted
        .iter()
        .filter(|&(unit, depth)| history.current().get(unit).is_some_and(|old| old < depth))
        .map(|(unit, _)| unit)
        .collect();
    // Any transition actually undone by a safety cut is consumed too.
    for (unit, depth) in wanted.iter() {
        for link in history.ancestry(unit) {
            let cursor = Cursor {
                unit,
                transition: link.transition,
            };
            if link.after < depth && !consumed.contains(&cursor) {
                consumed.insert(cursor);
                cursors.push(cursor);
            }
        }
    }
    Ok(Some(Retreat {
        profile: wanted.clone(),
        wanted,
        events: answered,
        cursors,
        stopped,
        adverse_units,
    }))
}
