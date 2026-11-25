use togi_journal::{Event, MemFs};
use togi_simrun::Persistence;

use crate::{Error, build, cut_trial};

pub(super) use togi_simrun::scripted::StagedDayGoal as Goal;

pub(super) fn build_journal(goal: Goal) -> Result<Vec<Event>, Error> {
    let persistence = Persistence::Memory(MemFs::new());
    let events = togi_simrun::scripted::staged_day(&persistence, &build(), goal)?;
    Ok(cut_trial(&events, |intent| goal.needs(intent))?.to_vec())
}
