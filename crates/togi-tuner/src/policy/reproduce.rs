//! A CPU-suspect event's one reproduction night: which event is next, what its attempts have shown, and the
//! commitments that open and close it.

use std::collections::{BTreeMap, HashSet};

use togi_journal::{
    Activity, Classification, DayEventId, ExposureClass, NightId, NightReproduction,
    ObservedProfile, Outcome, Queue, ReproductionState, ReproductionTarget, Seq, TaskId, Tested,
};
use togi_machine::{ContentId, WorkloadId};

use super::State;
use super::decision::{Decision, PolicyPayload};
use super::ledger::{Sig, is_adequate};
use super::night_query::NotReady;
use super::plan::workloads_of;
use crate::night::{
    Attempt, AttemptLog, AttemptNight, AttemptScope, Cell, Layout, Reproduction, ReproductionStage,
    reproduction_cells, reproduction_stage,
};

/// What the reproduction queue asks of this query.
#[derive(Clone, Debug)]
pub(super) enum ReproView {
    /// A reproduction commitment is owed now.
    Commit(Decision),
    /// The oldest open reproduction, if any, and the commitment that opens it.
    Stage(Option<Open>),
}

#[derive(Clone, Debug)]
pub(super) struct Open {
    pub(super) reproduction: Reproduction,
    pub(super) ordinal: u32,
    /// Owed before the first trial: the event's night is the night its work starts.
    pub(super) opening: Option<Decision>,
}

impl State {
    /// The latest `night.reproduction` of each source.
    fn reproduction_of(
        &self,
        source: &DayEventId,
    ) -> Option<&super::books::Committed<NightReproduction>> {
        self.nights
            .reproductions
            .iter()
            .rev()
            .find(|commit| &commit.payload.source == source)
    }

    /// The CPU-suspect events whose reproduction has not closed, oldest first, with each event's ordinal
    /// among all events.
    fn open_reproductions(&self) -> Vec<(u32, Seq, DayEventId, ObservedProfile)> {
        let mut events: Vec<_> = self.days.events.iter().collect();
        events.sort_by_key(|(_, book)| book.first);
        events
            .into_iter()
            .enumerate()
            .filter_map(|(index, (id, book))| {
                let event = &book.latest.payload;
                let suspect = matches!(event.classification, Classification::CpuSuspect { .. });
                let closed = self
                    .reproduction_of(id)
                    .is_some_and(|commit| commit.payload.state != ReproductionState::Pending);
                (suspect && !closed && !self.is_historical_event(id)).then(|| {
                    (
                        super::ledger::saturating_u32(u64::try_from(index).unwrap_or(u64::MAX)),
                        book.first,
                        id.clone(),
                        event.profile.clone(),
                    )
                })
            })
            .collect()
    }

    /// What an event's reproduction trials have shown: the log and the failing exposures.
    fn attempt_log(&self, attempts: &[Attempt], ordinal: u32) -> (AttemptLog, Vec<Seq>) {
        let task = TaskId {
            queue: Queue::Reproduction,
            ordinal,
        };
        let mut log = AttemptLog::default();
        let mut failures = Vec::new();
        for record in self.task_exposures(task) {
            let observed = &record.observed;
            if observed.activity != Activity::Reproduction {
                continue;
            }
            let scope = match observed.tested {
                Tested::Exact {} => AttemptScope::Exact,
                Tested::Isolated { ccd, .. } => AttemptScope::Unit {
                    unit: crate::UnitId::Ccd(ccd),
                },
            };
            match observed.outcome {
                Outcome::Failure => {
                    log.failed.insert(scope);
                    failures.push(record.seq);
                }
                Outcome::Pass if is_adequate(observed, &self.machine.cores) => {
                    let sig = Sig::of_class(&observed.class);
                    if let Some(cell) = attempts
                        .iter()
                        .filter(|attempt| attempt.scope == scope)
                        .flat_map(|attempt| attempt.matrix.fresh())
                        .find(|cell| Sig::of_cell(cell) == sig)
                    {
                        log.clean.insert((scope, *cell));
                    }
                }
                Outcome::Pass | Outcome::Inconclusive => {}
            }
        }
        (log, failures)
    }

    /// The classes of every cell the unfinished attempts still owe.
    fn outstanding_classes(
        attempts: &[Attempt],
        log: &AttemptLog,
        contents: &BTreeMap<WorkloadId, ContentId>,
    ) -> Result<Vec<ExposureClass>, NotReady> {
        let mut seen: HashSet<(AttemptScope, Cell)> = HashSet::new();
        let mut classes = Vec::new();
        for attempt in attempts
            .iter()
            .filter(|attempt| !log.failed.contains(&attempt.scope))
        {
            for cell in attempt.matrix.fresh() {
                if log.clean.contains(&(attempt.scope, *cell))
                    || !seen.insert((attempt.scope, *cell))
                {
                    continue;
                }
                classes
                    .push(Self::class_of(cell, contents).ok_or_else(|| {
                        NotReady::UnknownContent(workloads_of(cell.workloads)[0])
                    })?);
            }
        }
        Ok(classes)
    }

    fn reproduction_consumed(&self, ordinal: u32) -> u64 {
        let task = TaskId {
            queue: Queue::Reproduction,
            ordinal,
        };
        self.task_trials(task).map(Self::trial_wall).sum()
    }

    /// The reproduction queue while `current` is the open night, `None` between nights. An event's one
    /// night is the night its reproduction opened in; once that night closed, its attempt is over.
    pub(super) fn reproduction_view(
        &self,
        layout: &Layout<'_>,
        current: Option<NightId>,
        contents: &BTreeMap<WorkloadId, ContentId>,
    ) -> Result<ReproView, NotReady> {
        for (ordinal, event, source, profile) in self.open_reproductions() {
            let ccd_profile = match &profile {
                ObservedProfile::Ccd { ccd, .. } => crate::UnitProfile::of_ccd(ccd),
                // Nonuniform offsets are observations, not eligible profiles (tuner.md), so no night
                // writes one: the task closes at once as untestable and the veto still compares the
                // event on every physical core.
                ObservedProfile::PerCore { .. } => {
                    let Some(night) = current else {
                        continue;
                    };
                    return Ok(ReproView::Commit(self.close_reproduction(
                        &source,
                        ordinal,
                        night,
                        None,
                        Vec::new(),
                        Vec::new(),
                        ReproductionState::Untestable,
                    )?));
                }
            };
            // Stock is observation-only; no workload can reproduce an all-zero profile.
            if ccd_profile.is_zero() {
                let Some(night) = current else {
                    continue;
                };
                return Ok(ReproView::Commit(self.close_reproduction(
                    &source,
                    ordinal,
                    night,
                    None,
                    Vec::new(),
                    Vec::new(),
                    ReproductionState::Untestable,
                )?));
            }
            let committed = self.reproduction_of(&source);
            let Some(attempt_night) = committed.map(|commit| commit.payload.night).or(current)
            else {
                continue;
            };
            let night_over = current != Some(attempt_night);
            let attempts = reproduction_cells(layout, &ccd_profile, ordinal)?;
            let (log, failures) = self.attempt_log(&attempts, ordinal);
            let stage = reproduction_stage(
                event,
                &attempts,
                &log,
                if night_over {
                    AttemptNight::Earlier
                } else {
                    AttemptNight::This
                },
            );
            let target = Some(&ccd_profile);
            let state = match &stage.stage {
                ReproductionStage::Concluded => Some(if log.failed.is_empty() {
                    ReproductionState::Unreproduced
                } else {
                    ReproductionState::Reproduced
                }),
                ReproductionStage::Lapsed => Some(ReproductionState::Unreproduced),
                ReproductionStage::Open(_) => None,
            };
            if let Some(state) = state {
                let remaining = Self::outstanding_classes(&attempts, &log, contents)?;
                return Ok(ReproView::Commit(self.close_reproduction(
                    &source,
                    ordinal,
                    attempt_night,
                    target,
                    remaining,
                    failures,
                    state,
                )?));
            }
            if night_over {
                continue;
            }
            let opening = if committed.is_none() {
                let remaining = Self::outstanding_classes(&attempts, &log, contents)?;
                Some(self.reproduction_payload(
                    &source,
                    ordinal,
                    attempt_night,
                    target,
                    remaining,
                    Vec::new(),
                    ReproductionState::Pending,
                )?)
            } else {
                None
            };
            return Ok(ReproView::Stage(Some(Open {
                reproduction: stage,
                ordinal,
                opening: opening.map(|payload| Decision::new(payload, vec![event])),
            })));
        }
        Ok(ReproView::Stage(None))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one reproduction commitment's fields"
    )]
    fn reproduction_payload(
        &self,
        source: &DayEventId,
        ordinal: u32,
        night: NightId,
        profile: Option<&crate::UnitProfile>,
        remaining: Vec<ExposureClass>,
        failures: Vec<Seq>,
        state: ReproductionState,
    ) -> Result<PolicyPayload, NotReady> {
        let target = profile
            .map(|profile| {
                Ok::<_, NotReady>(ReproductionTarget::Exact {
                    profile: self.ccd_vector(profile)?,
                })
            })
            .transpose()?;
        Ok(PolicyPayload::NightReproduction(NightReproduction {
            source: source.clone(),
            target,
            night,
            consumed_s: self.reproduction_consumed(ordinal),
            remaining,
            failures,
            state,
        }))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one reproduction commitment's fields"
    )]
    fn close_reproduction(
        &self,
        source: &DayEventId,
        ordinal: u32,
        night: NightId,
        profile: Option<&crate::UnitProfile>,
        remaining: Vec<ExposureClass>,
        failures: Vec<Seq>,
        state: ReproductionState,
    ) -> Result<Decision, NotReady> {
        let causes = failures.clone();
        let payload =
            self.reproduction_payload(source, ordinal, night, profile, remaining, failures, state)?;
        let mut causes = causes;
        causes.extend(self.days.events.get(source).map(|book| book.first));
        causes.extend(self.night_closed_by(night));
        Ok(Decision::new(payload, causes))
    }
}
