use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use togi_defect::Entry;
use togi_facts::{Fact, SessionFacts};
use togi_journal::{CarriedCore, CarriedSource, Event, Payload, ResetTarget, SessionId};
use togi_machine::{BiosContext, CoreSet};

use crate::archives::{Archives, CarryError};
use crate::seeds::Exclusions;
use crate::summary::{SourceSummary, scan_exclusions, summarize};

/// What the sources hold, before the session's actual BIOS context, backends and evidence epoch are known.
///
/// It is not facts: [`PendingCarry::resolve`] is the only way to get them.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingCarry {
    pub(crate) sources: Vec<CarriedSource>,
    pub(crate) context: Option<BiosContext>,
    pub(crate) seeds: Vec<CarriedCore>,
    pub(crate) facts: Vec<Fact>,
}

impl PendingCarry {
    /// The sources the seed walk read, newest first.
    pub fn sources(&self) -> &[CarriedSource] {
        &self.sources
    }

    /// The BIOS context the newest source recorded; `None` when it recorded none.
    pub const fn context(&self) -> Option<&BiosContext> {
        self.context.as_ref()
    }

    /// Per core, ascending, the deepest alone pass and the shallowest confirmed failure point of all sources.
    pub fn seeds(&self) -> &[CarriedCore] {
        &self.seeds
    }

    /// The facts still eligible before the session's BIOS context, evidence epoch and backends are known,
    /// chronological by original session then sequence.
    pub fn fact_candidates(&self) -> &[Fact] {
        &self.facts
    }
}

/// Whether the live journal has already committed the carry of `pending` or started its phases, past the point
/// where a carry can apply. The session asks before [`select`].
pub fn settled<'a>(pending: SessionId, live: impl IntoIterator<Item = &'a Event>) -> bool {
    live.into_iter().any(|event| match &event.payload {
        Payload::SessionCarried(carried) => carried
            .sources
            .first()
            .is_some_and(|source| source.session == pending),
        Payload::CorePhase(_) => true,
        Payload::SessionStart(_)
        | Payload::SessionContext(_)
        | Payload::SessionBaseline(_)
        | Payload::SessionNotice(_)
        | Payload::SessionWarning(_)
        | Payload::SessionArchived(_)
        | Payload::ConfigLoaded(_)
        | Payload::PreflightCheck(_)
        | Payload::SmuIntent(_)
        | Payload::SmuWrite(_)
        | Payload::SmuReadback(_)
        | Payload::SmuError(_)
        | Payload::ProfileApplied(_)
        | Payload::ProfileChange(_)
        | Payload::ProfileRestored(_)
        | Payload::TrialIntent(_)
        | Payload::TrialStart(_)
        | Payload::TrialProgress(_)
        | Payload::TrialSignal(_)
        | Payload::TrialSample(_)
        | Payload::TrialEnd(_)
        | Payload::TrialCarried(_)
        | Payload::Failure(_)
        | Payload::FailureCarried(_)
        | Payload::Mce(_)
        | Payload::CrashDetected(_)
        | Payload::TunerDecision(_)
        | Payload::CheckingCycle(_)
        | Payload::CheckingStep(_)
        | Payload::CheckingChain(_)
        | Payload::HostRanking(_)
        | Payload::HuntStart(_)
        | Payload::HuntGroup(_)
        | Payload::HuntEnd(_)
        | Payload::HuntSkipped(_)
        | Payload::Combination(_)
        | Payload::DeepeningRound(_)
        | Payload::TunerWarning(_)
        | Payload::BackendRetry(_)
        | Payload::CommandReset(_)
        | Payload::DefectFound(_)
        | Payload::DefectAnswered(_)
        | Payload::DeadEnd(_)
        | Payload::BootSavedEntry(_)
        | Payload::BootLeaveReason(_)
        | Payload::Shutdown(_)
        | Payload::JournalTorn(_)
        | Payload::StateRebuilt(_) => false,
    })
}

/// Selects what a transition carries from `chain`: the marked source first, then the older archives, newest
/// first. A source is read only when a walk needs it, once, and one at or before `boundary` never. `None` when the
/// marked source is itself behind the boundary.
pub fn select<A: Archives>(
    chain: &[SessionId],
    boundary: Option<SessionId>,
    registry: &'static [Entry],
    archives: &mut A,
) -> Result<Option<PendingCarry>, CarryError<A::Error>> {
    let Some(&marked) = chain.first() else {
        return Ok(None);
    };
    let mut walk = Walk {
        chain,
        boundary,
        registry,
        archives,
        summaries: BTreeMap::new(),
        originals: BTreeMap::new(),
    };
    if walk.behind(marked) {
        return Ok(None);
    }
    let first = walk.summary(marked)?;
    let seed_sources = walk.seed_sources(&first)?;
    let facts = walk.facts(&first)?;
    Ok(Some(PendingCarry {
        sources: seed_sources
            .iter()
            .map(|source| carried_source(source.facts()))
            .collect(),
        context: first.context().cloned(),
        seeds: merge_seeds(&seed_sources),
        facts,
    }))
}

fn carried_source(facts: &SessionFacts) -> CarriedSource {
    CarriedSource {
        session: facts.id,
        schema: facts.build.schema,
        ruleset: facts.build.ruleset,
    }
}

fn merge_seeds(sources: &[Rc<SourceSummary>]) -> Vec<CarriedCore> {
    let mut merged: BTreeMap<_, CarriedCore> = BTreeMap::new();
    for source in sources {
        let seeds = source.seeds();
        for (&core, &limit) in &seeds.limits {
            let entry = merged.entry(core).or_insert_with(|| CarriedCore {
                core,
                candidate_solo_limit: None,
                failure_point: None,
            });
            if entry
                .candidate_solo_limit
                .is_none_or(|held| limit.offset < held.offset)
            {
                entry.candidate_solo_limit = Some(limit);
            }
        }
        for (&core, &point) in &seeds.points {
            let entry = merged.entry(core).or_insert_with(|| CarriedCore {
                core,
                candidate_solo_limit: None,
                failure_point: None,
            });
            if entry
                .failure_point
                .is_none_or(|held| point.offset > held.offset)
            {
                entry.failure_point = Some(point);
            }
        }
    }
    merged.into_values().collect()
}

struct Walk<'a, A: Archives> {
    chain: &'a [SessionId],
    boundary: Option<SessionId>,
    registry: &'static [Entry],
    archives: &'a mut A,
    summaries: BTreeMap<SessionId, Rc<SourceSummary>>,
    originals: BTreeMap<SessionId, Option<Exclusions>>,
}

impl<A: Archives> Walk<'_, A> {
    fn behind(&self, session: SessionId) -> bool {
        self.boundary.is_some_and(|boundary| session <= boundary)
    }

    fn summary(&mut self, session: SessionId) -> Result<Rc<SourceSummary>, CarryError<A::Error>> {
        if let Some(summary) = self.summaries.get(&session) {
            return Ok(Rc::clone(summary));
        }
        let events = self
            .archives
            .events(session)
            .map_err(|source| CarryError::Open { session, source })?
            .ok_or(CarryError::Missing { session })?;
        let summary = Rc::new(
            summarize(session, events, self.registry, self.boundary)
                .map_err(|source| CarryError::Read { session, source })?,
        );
        self.summaries.insert(session, Rc::clone(&summary));
        Ok(summary)
    }

    fn seed_sources(
        &mut self,
        first: &Rc<SourceSummary>,
    ) -> Result<Vec<Rc<SourceSummary>>, CarryError<A::Error>> {
        let mut sources = vec![Rc::clone(first)];
        if first.carry_commit().is_some()
            || first.context().is_none()
            || first.reset_all().is_some()
        {
            return Ok(sources);
        }
        for &session in &self.chain[1..] {
            if self.behind(session) {
                break;
            }
            let source = self.summary(session)?;
            let previous = sources
                .last()
                .map(|previous| previous.facts().build.clone());
            let repeats = previous.is_some_and(|previous| {
                let build = &source.facts().build;
                build.ruleset == previous.ruleset && build.evidence == previous.evidence
            });
            if source.context() != first.context() || repeats {
                break;
            }
            let last = source.carry_commit().is_some() || source.reset_all().is_some();
            sources.push(source);
            if last {
                break;
            }
        }
        Ok(sources)
    }

    fn facts(&mut self, first: &Rc<SourceSummary>) -> Result<Vec<Fact>, CarryError<A::Error>> {
        let mut selected = Vec::new();
        if first.context().is_none() {
            return Ok(selected);
        }
        let mut cleared = CoreSet::default();
        let mut seen = HashSet::new();
        let mut source = Rc::clone(first);
        let mut index = 0;
        loop {
            let session = source.facts();
            for fact in session.facts.iter().chain(&session.carried) {
                if self.behind(fact.source.session) {
                    continue;
                }
                let id = fact.id();
                if seen.contains(&id) || !eligible(session, fact, cleared) {
                    continue;
                }
                if self.excludes(fact)? {
                    continue;
                }
                seen.insert(id);
                selected.push(fact.clone());
            }
            let committed = !session.carried.is_empty() && source.carry_commit().is_some();
            if source.reset_all().is_some() || committed {
                break;
            }
            for reset in &session.resets {
                if let ResetTarget::Core(core) = reset.target {
                    cleared.insert(core);
                }
            }
            index += 1;
            let Some(&next) = self.chain.get(index) else {
                break;
            };
            if self.behind(next) {
                break;
            }
            let older = self.summary(next)?;
            if older.context() != first.context() {
                break;
            }
            source = older;
        }
        selected.sort_by_key(Fact::id);
        Ok(selected)
    }

    fn excludes(&mut self, fact: &Fact) -> Result<bool, CarryError<A::Error>> {
        if !fact.is_failure() || self.registry.is_empty() {
            return Ok(false);
        }
        let original = fact.source.session;
        if self.chain.contains(&original) {
            return Ok(self.summary(original)?.exclusions().excludes(fact));
        }
        if !self.originals.contains_key(&original) {
            let events = self
                .archives
                .events(original)
                .map_err(|source| CarryError::Open {
                    session: original,
                    source,
                })?;
            let scanned = match events {
                None => None,
                Some(events) => Some(scan_exclusions(events, self.registry).map_err(|source| {
                    CarryError::Read {
                        session: original,
                        source,
                    }
                })?),
            };
            self.originals.insert(original, scanned);
        }
        Ok(self.originals[&original]
            .as_ref()
            .is_some_and(|exclusions| exclusions.excludes(fact)))
    }
}

fn eligible(source: &SessionFacts, fact: &Fact, cleared: CoreSet) -> bool {
    let at = fact.local;
    if source.cutoffs.all().is_some_and(|reset| at <= reset) {
        return false;
    }
    !fact.loaded().iter().any(|core| {
        cleared.contains(core) || source.cutoffs.core(core).is_some_and(|reset| at <= reset)
    })
}
