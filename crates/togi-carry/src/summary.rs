use togi_defect::Entry;
use togi_facts::{FactsError, SessionFacts, extract};
use togi_journal::{Event, Seq, SessionId};
use togi_machine::BiosContext;

use crate::seeds::{ExclusionTracker, Exclusions, SeedFold, SourceSeeds};

/// One archived session folded once: its facts, the failures its own decisions excluded as defect-affected, and the
/// seed candidates it yields. Its size follows the distinct evidence the session holds, not its event count.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceSummary {
    facts: SessionFacts,
    exclusions: Exclusions,
    seeds: SourceSeeds,
}

impl SourceSummary {
    pub const fn facts(&self) -> &SessionFacts {
        &self.facts
    }

    pub const fn context(&self) -> Option<&BiosContext> {
        self.facts.context.as_ref()
    }

    pub const fn carry_commit(&self) -> Option<Seq> {
        self.facts.carry_commit
    }

    /// The latest `command.reset` with `all` in this source. Its seeds and facts older than it are gone, and no
    /// older source contributes.
    pub const fn reset_all(&self) -> Option<Seq> {
        self.facts.cutoffs.all()
    }

    pub(crate) const fn seeds(&self) -> &SourceSeeds {
        &self.seeds
    }

    pub(crate) const fn exclusions(&self) -> &Exclusions {
        &self.exclusions
    }

    #[cfg(test)]
    pub(crate) fn retained_seeds(&self) -> usize {
        crate::seeds::retained(&self.seeds, &self.exclusions)
    }
}

pub(crate) fn summarize<E>(
    session: SessionId,
    events: impl IntoIterator<Item = Result<Event, E>>,
    registry: &'static [Entry],
    boundary: Option<SessionId>,
) -> Result<SourceSummary, FactsError<E>> {
    let mut seeds = SeedFold::new(session, boundary);
    let mut tracker = ExclusionTracker::new(registry);
    let tee = events.into_iter().inspect(|event| {
        if let Ok(event) = event {
            seeds.fold(event);
            tracker.fold(event);
        }
    });
    let facts = extract(session, tee)?;
    let seeds = seeds.finish(&facts, &tracker);
    Ok(SourceSummary {
        facts,
        exclusions: tracker.finish(),
        seeds,
    })
}

pub(crate) fn scan_exclusions<E>(
    events: impl IntoIterator<Item = Result<Event, E>>,
    registry: &'static [Entry],
) -> Result<Exclusions, E> {
    let mut tracker = ExclusionTracker::new(registry);
    for event in events {
        tracker.fold(&event?);
    }
    Ok(tracker.finish())
}
