//! `State::next`: drains the commitments the night owes, then asks the scheduler for the next trial.

use std::collections::{BTreeMap, HashSet};

use togi_journal::{
    Activity, Budget, DeadEnd, DeadEndCondition, NightId, NightPlan, NightQualification,
    PlannedTask, QualificationState, Queue as JournalQueue, Seq, TaskId, TaskStatus, Tested,
    UnavailableBackend,
};
use togi_machine::{CcdId, CcdProfile, ContentId, TargetOffset, WorkloadId};

use super::State;
use super::decision::{Decision, PolicyPayload};
use super::ledger::{Sig, is_adequate};
use super::mapping::MappingPlan;
use super::night_query::{Action, EndReason, NightQuery, NotReady, runnable};
use super::plan::TrialPlan;
use super::qualify::QualWork;
use super::reproduce::{Open, ReproView};
use crate::cap::EDGE_MARGIN;
use crate::night::{
    AttemptScope, Cell, Consumed, EdgeU, Inputs, Layout, Matrix, NightTask, Queue, Reason,
    Reproduction, ReproductionStage, Reuse, Stock, WallSecs, Work, mapping_cells, next_task,
    qualification_cells,
};

const fn activity_of(queue: Queue) -> Activity {
    match queue {
        Queue::Reproduction => Activity::Reproduction,
        Queue::Qualification | Queue::Requalification => Activity::Qualification,
        Queue::Mapping => Activity::Mapping,
        Queue::Controls => Activity::Control,
    }
}

/// The plan's queues, the filler last, with the scheduler queue each one's share belongs to.
const PLAN_QUEUES: [(JournalQueue, Option<Queue>); 6] = [
    (JournalQueue::Reproduction, Some(Queue::Reproduction)),
    (JournalQueue::Qualification, Some(Queue::Qualification)),
    (JournalQueue::Mapping, Some(Queue::Mapping)),
    (JournalQueue::Control, Some(Queue::Controls)),
    (JournalQueue::Requalification, Some(Queue::Requalification)),
    (JournalQueue::Filler, None),
];

/// Each night's trials as read so far.
pub(super) type NightTallies = BTreeMap<NightId, NightTally>;

/// A night's trials as read so far: each queue's wall seconds, the tasks in order of first trial, and
/// the trials still without an end. A night's trials only append and a trial ends once, so each is
/// read once and its wall added when it ends.
#[derive(Clone, Debug, Default)]
pub(super) struct NightTally {
    read: usize,
    secs: BTreeMap<JournalQueue, u64>,
    tasks: Vec<TaskId>,
    unended: Vec<usize>,
}

impl NightTally {
    fn consumed(&self) -> Consumed {
        let of = |queue: JournalQueue| WallSecs::new(self.secs.get(&queue).copied().unwrap_or(0));
        Consumed {
            reproduction: of(JournalQueue::Reproduction),
            qualification: of(JournalQueue::Qualification),
            mapping: of(JournalQueue::Mapping),
            controls: of(JournalQueue::Control),
            requalification: of(JournalQueue::Requalification),
            filler: of(JournalQueue::Filler),
        }
    }
}

/// `work` with only the cells no excluded backend blocks; `None` when none is left.
fn runnable_work(
    work: Option<Work>,
    unavailable: &BTreeMap<togi_machine::Backend, Vec<Seq>>,
) -> Option<Work> {
    let mut work = work?;
    work.cells.retain(|cell| runnable(cell, unavailable));
    (!work.cells.is_empty()).then_some(work)
}

/// The runnable cell at `cursor` in `matrix`'s rotation: the first runnable cell from the cursor's place
/// onward, wrapping, so a cell that needs an excluded backend is skipped, not consumed.
fn rotated(
    matrix: &Matrix,
    cursor: u32,
    unavailable: &BTreeMap<togi_machine::Backend, Vec<Seq>>,
) -> Option<Cell> {
    let cells: Vec<Cell> = matrix.fresh().copied().collect();
    let start = usize::try_from(cursor).unwrap_or(usize::MAX) % cells.len().max(1);
    cells
        .iter()
        .cycle()
        .skip(start)
        .take(cells.len())
        .find(|cell| runnable(cell, unavailable))
        .copied()
}

/// The query's excluded backends as the plan records them.
fn unavailable_of(
    unavailable: &BTreeMap<togi_machine::Backend, Vec<Seq>>,
) -> Vec<UnavailableBackend> {
    unavailable
        .iter()
        .map(|(backend, causes)| UnavailableBackend {
            backend: *backend,
            causes: causes.clone(),
        })
        .collect()
}

/// A trial the night chose, before the plan commitment that names its task.
struct Chosen {
    task: TaskId,
    activity: Activity,
    cell: Cell,
    profile: CcdProfile,
    tested: Tested,
    retry: bool,
}

/// The evidence a qualification commitment cites.
fn qualification_causes(qualification: &NightQualification) -> Vec<Seq> {
    let witnesses = match &qualification.state {
        QualificationState::Start { .. } => return Vec::new(),
        QualificationState::Progress { witnesses, .. }
        | QualificationState::Complete { witnesses }
        | QualificationState::Failed { witnesses, .. } => witnesses,
    };
    let mut seqs: Vec<Seq> = witnesses
        .iter()
        .map(|witness| match witness.evidence {
            togi_journal::CellEvidence::Fresh { exposure }
            | togi_journal::CellEvidence::Reused { exposure } => exposure,
        })
        .collect();
    if let QualificationState::Failed { failure, .. } = &qualification.state {
        seqs.push(*failure);
    }
    seqs
}

impl State {
    /// What the night does next. Every owed commitment comes back one at a time (append it, fold it, ask
    /// again) before the next trial. A restarted query continues the open night, its consumption and its
    /// task, all derived from the journal (a crash reboot back into tuning continues it); with no night
    /// open, the commitments the last night owes at its close come first, then the plan that opens a new one. The night ends only at a dead end or when every
    /// runnable cell needs an excluded backend: otherwise there is always a trial to run.
    pub fn next(&self, query: &NightQuery<'_>) -> Result<Action, NotReady> {
        // Evidence is current only under the contents this night runs: a changed content invalidates the
        // earlier exposures before any trial of it has been folded.
        let changed = query.contents.map().iter().any(|(workload, content)| {
            self.machine
                .contents
                .get(workload)
                .is_some_and(|known| known != content)
        });
        if changed {
            let mut view = self.clone();
            for (&workload, content) in query.contents.map() {
                view.machine.set_content(workload, content);
            }
            return view.next_in(query);
        }
        self.next_in(query)
    }

    fn next_in(&self, query: &NightQuery<'_>) -> Result<Action, NotReady> {
        if self.machine.session.is_none() {
            return Err(NotReady::NoSession);
        }
        if self.machine.firmware.is_none() {
            return Err(NotReady::NoFirmware);
        }
        if self.days.history.is_none() {
            return Err(NotReady::NoBaseline);
        }
        if self.stop.is_some() {
            return Ok(Action::EndNight(EndReason::DeadEnd));
        }
        if let Some(owed) = [
            self.owed_failure(),
            self.owed_exposure(),
            self.owed_warning(),
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return Ok(Action::Decide(owed));
        }
        let known = self.known_contents(Some(query.contents.map()));
        self.with_layout(|layout| {
            let mapping = self.mapping_plan(layout, &known)?;
            if let Some(owed) = self.owed_edge(&mapping) {
                return Ok(Action::Decide(owed));
            }
            if let Some(owed) = self.owed_cap(&mapping)? {
                return Ok(Action::Decide(owed));
            }
            if let Some(owed) = self.closure_owed(layout, &known)? {
                return Ok(Action::Decide(owed));
            }
            self.schedule(query, layout, &known, &mapping)
        })
    }

    fn qualification_decision(payload: NightQualification) -> Decision {
        let causes = qualification_causes(&payload);
        Decision::new(PolicyPayload::NightQualification(payload), causes)
    }

    /// The progress or terminal commitment of an open or concluded qualification.
    fn qualification_commit_owed(&self, progress: bool) -> Result<Option<Decision>, NotReady> {
        for record in self.qualification_records().iter() {
            if !self.is_current_record(record) {
                continue;
            }
            if let Some(payload) = self.qualification_owed(record, progress)? {
                return Ok(Some(Self::qualification_decision(payload)));
            }
        }
        Ok(None)
    }

    /// The trials of one task, in intent order.
    pub(super) fn task_trials(&self, task: TaskId) -> impl Iterator<Item = &super::books::Trial> {
        self.trials
            .by_task
            .get(&task)
            .into_iter()
            .flatten()
            .filter_map(|&at| self.trials.all.get(at))
    }

    /// `f` of the tally of `night`'s trials, brought up to date with the trials folded since it was last
    /// read.
    fn night_tally<R>(&self, night: NightId, f: impl FnOnce(&NightTally) -> R) -> R {
        let trials: &[usize] = self.trials.by_night.get(&night).map_or(&[], Vec::as_slice);
        let mut tallies = self.caches.nights.lock();
        let tally = tallies.entry(night).or_default();
        if tally.read > trials.len() {
            *tally = NightTally::default();
        }
        let NightTally {
            read,
            secs,
            tasks,
            unended,
        } = &mut *tally;
        for &at in trials.get(*read..).unwrap_or_default() {
            let Some(task) = self.trials.all.get(at).and_then(super::books::Trial::task) else {
                continue;
            };
            if !tasks.contains(&task) {
                tasks.push(task);
            }
            unended.push(at);
        }
        *read = trials.len();
        unended.retain(|&at| {
            let Some(trial) = self.trials.all.get(at) else {
                return false;
            };
            match (trial.task(), trial.end.is_some()) {
                (Some(task), true) => {
                    *secs.entry(task.queue).or_default() += Self::trial_wall(trial);
                    false
                }
                (Some(_), false) => true,
                (None, _) => false,
            }
        });
        f(tally)
    }

    fn consumed(&self, night: NightId) -> Consumed {
        self.night_tally(night, NightTally::consumed)
    }

    /// The `day.boot_start` that closed `night`, when it is closed.
    pub(super) fn night_closed_by(&self, night: NightId) -> Option<Seq> {
        self.nights
            .plans
            .get(&night)
            .and_then(|book| book.closed_by)
    }

    /// How many nights opened before `night`: the ordinal of its filler task.
    fn night_ordinal(&self, night: NightId) -> u32 {
        super::ledger::saturating_u32(
            u64::try_from(self.nights.plans.range(..night).count()).unwrap_or(u64::MAX),
        )
    }

    /// The plan of `night` as it stands (`None` for the plan that opens a night): each queue's share and
    /// consumption, its tasks with `current` running and the backends it runs no cell on.
    fn plan_payload(
        &self,
        night: Option<NightId>,
        current: Option<TaskId>,
        unavailable: Vec<UnavailableBackend>,
    ) -> NightPlan {
        let (consumed, mut tasks): (Consumed, Vec<PlannedTask>) = match night {
            Some(night) => self.night_tally(night, |tally| {
                (
                    tally.consumed(),
                    tally
                        .tasks
                        .iter()
                        .map(|&task| PlannedTask {
                            task,
                            status: TaskStatus::Complete,
                        })
                        .collect(),
                )
            }),
            None => (Consumed::default(), Vec::new()),
        };
        let budgets = PLAN_QUEUES
            .into_iter()
            .map(|(queue, share)| Budget {
                queue,
                share_pct: share.map_or(0, |share| self.params.budgets.share(share)),
                consumed_s: match share {
                    Some(share) => consumed.get(share).get(),
                    None => consumed.filler.get(),
                },
            })
            .collect();
        if let Some(current) = current {
            match tasks.iter_mut().find(|planned| planned.task == current) {
                Some(planned) => planned.status = TaskStatus::Running,
                None => tasks.push(PlannedTask {
                    task: current,
                    status: TaskStatus::Running,
                }),
            }
        }
        NightPlan {
            night,
            budgets,
            tasks,
            unavailable,
        }
    }

    /// The plan commitment owed for `night`: one whose task list or unavailable backends moved (`whole`
    /// false), or, at its close, one whose consumption moved too (`whole` true). It cites the events that
    /// excluded each unavailable backend.
    fn plan_owed(
        &self,
        night: NightId,
        current: Option<TaskId>,
        whole: bool,
        unavailable: Vec<UnavailableBackend>,
    ) -> Option<Decision> {
        let desired = self.plan_payload(Some(night), current, unavailable);
        let latest = &self.nights.plans.get(&night)?.latest.payload;
        let owed = latest.tasks != desired.tasks
            || latest.unavailable != desired.unavailable
            || (whole && latest.budgets != desired.budgets);
        owed.then(|| {
            let mut causes: Vec<Seq> = self.last.into_iter().collect();
            causes.extend(self.night_closed_by(night));
            causes.extend(
                desired
                    .unavailable
                    .iter()
                    .flat_map(|excluded| excluded.causes.iter().copied()),
            );
            causes.sort_unstable();
            causes.dedup();
            Decision::new(PolicyPayload::NightPlan(desired), causes)
        })
    }

    /// What the last night owes once it closed (a later boot's `day.boot_start`, the operator's stop or a
    /// session reset): its reproduction marked unreproduced, its qualification progress with the cells still
    /// outstanding and its final plan, each citing the event that closed it. Appended by the next query: the
    /// day invocation or the next night.
    pub(super) fn closure_owed(
        &self,
        layout: &Layout<'_>,
        known: &BTreeMap<WorkloadId, ContentId>,
    ) -> Result<Option<Decision>, NotReady> {
        if self.nights.open.is_some() {
            return Ok(None);
        }
        let Some((&night, book)) = self.nights.plans.iter().next_back() else {
            return Ok(None);
        };
        let Some(closed_by) = book.closed_by else {
            return Ok(None);
        };
        if let ReproView::Commit(owed) = self.reproduction_view(layout, None, known)? {
            return Ok(Some(owed));
        }
        if let Some(mut owed) = self.qualification_commit_owed(true)? {
            owed.causes.push(closed_by);
            return Ok(Some(owed));
        }
        Ok(self.plan_owed(night, None, true, book.latest.payload.unavailable.clone()))
    }

    fn mapping_work(
        &self,
        layout: &Layout<'_>,
        plan: &MappingPlan,
    ) -> Result<Option<(Work, TaskId)>, NotReady> {
        let Some(target) = plan.target else {
            return Ok(None);
        };
        let ordinal = plan.mapped.ordinal_for(&target);
        let task = TaskId {
            queue: JournalQueue::Mapping,
            ordinal,
        };
        let matrix = mapping_cells(layout, target.ccd, target.offset, ordinal)?;
        let done: HashSet<Sig> = self
            .task_exposures(task)
            .filter(|record| {
                record.observed.outcome == togi_journal::Outcome::Pass
                    && is_adequate(&record.observed, &self.machine.cores)
            })
            .map(|record| Sig::of_class(&record.observed.class))
            .collect();
        let cells: Vec<_> = matrix
            .fresh()
            .filter(|cell| !done.contains(&Sig::of_cell(cell)))
            .copied()
            .collect();
        if cells.is_empty() {
            return Ok(None);
        }
        Ok(Some((
            Work {
                profile: matrix.profile,
                cells,
                reason: Reason::Mapping { target },
            },
            task,
        )))
    }

    fn dead_end(&self, class: &crate::night::Cell) -> Decision {
        Decision::new(
            PolicyPayload::DeadEnd(DeadEnd {
                condition: DeadEndCondition::FailureAtZero,
                core: None,
                detail: format!("platform unstable at stock under {:?}", class.regime()),
                action: None,
            }),
            self.last.into_iter().collect(),
        )
    }

    /// The open night's next trial, or the commitment it owes first.
    fn schedule(
        &self,
        query: &NightQuery<'_>,
        layout: &Layout<'_>,
        known: &BTreeMap<WorkloadId, ContentId>,
        mapping: &MappingPlan,
    ) -> Result<Action, NotReady> {
        if let Some(owed) = self.qualification_commit_owed(false)? {
            return Ok(Action::Decide(owed));
        }
        let reproduction = self.reproduction_view(layout, self.nights.open, known)?;
        let open: Option<Open> = match reproduction {
            ReproView::Commit(owed) => return Ok(Action::Decide(owed)),
            ReproView::Stage(open) => open,
        };
        if let Some(owed) = self.owed_day_events(query.now)?.into_iter().next() {
            return Ok(Action::Decide(owed));
        }
        if let Some(owed) = self.owed_day_vetoes()?.into_iter().next() {
            return Ok(Action::Decide(owed));
        }
        let excluded = unavailable_of(query.unavailable);
        let Some(night) = self.nights.open else {
            let mut causes: Vec<Seq> = self.last.into_iter().collect();
            causes.extend(
                excluded
                    .iter()
                    .flat_map(|backend| backend.causes.iter().copied()),
            );
            causes.sort_unstable();
            causes.dedup();
            return Ok(Action::Decide(Decision::new(
                PolicyPayload::NightPlan(self.plan_payload(None, None, excluded)),
                causes,
            )));
        };
        let stock = self.stock(layout)?;
        if let Stock::Unstable { class } = &stock {
            return Ok(Action::Decide(self.dead_end(class)));
        }
        let (next, requal, due) = self.qualification_work(query.now)?;
        if matches!(stock, Stock::Cleared { .. }) {
            for work in [&next, &requal] {
                if let QualWork::NeedStart { profile, purpose } = work {
                    return Ok(Action::Decide(
                        self.start_qualification(profile, *purpose, known)?,
                    ));
                }
            }
        }
        let unavailable = query.unavailable;
        let stock = match stock {
            Stock::Pending { cause, work } => match runnable_work(Some(work), unavailable) {
                Some(work) => Stock::Pending { cause, work },
                // Controls must complete before any nonzero work, and none of theirs can run.
                None => return Ok(self.end_unavailable(night, excluded)),
            },
            Stock::Cleared { rerun } => Stock::Cleared {
                rerun: runnable_work(rerun, unavailable),
            },
            Stock::Unstable { class } => Stock::Unstable { class },
        };
        let qualification_of = |work: &QualWork, reason: Reason| match work {
            QualWork::Open { record, remaining } => runnable_work(
                Some(Work {
                    profile: record.start.payload.profile.ccd.clone(),
                    cells: remaining.clone(),
                    reason,
                }),
                unavailable,
            )
            .map(|work| (work, record.task())),
            QualWork::None | QualWork::NeedStart { .. } => None,
        };
        let qualification = qualification_of(&next, Reason::Qualification);
        let requalification =
            due.and_then(|due| qualification_of(&requal, Reason::Requalification { due }));
        let mapping_work = self
            .mapping_work(layout, mapping)?
            .and_then(|(work, task)| {
                runnable_work(Some(work), unavailable).map(|work| (work, task))
            });
        let reproduction = open
            .as_ref()
            .and_then(|open| match &open.reproduction.stage {
                ReproductionStage::Open(work) => runnable_work(Some(work.clone()), unavailable)
                    .map(|work| Reproduction {
                        event: open.reproduction.event,
                        stage: ReproductionStage::Open(work),
                    }),
                ReproductionStage::Concluded | ReproductionStage::Lapsed => {
                    Some(open.reproduction.clone())
                }
            });
        let inputs = Inputs {
            budgets: self.params.budgets,
            consumed: self.consumed(night),
            stock,
            reproduction,
            qualification: qualification.as_ref().map(|(work, _)| work.clone()),
            mapping: mapping_work.as_ref().map(|(work, _)| work.clone()),
            requalification: requalification.as_ref().map(|(work, _)| work.clone()),
        };
        let chosen = match next_task(&inputs) {
            NightTask::Run {
                queue,
                cell,
                profile,
                reason,
            } => {
                if let Some(opening) = open
                    .as_ref()
                    .filter(|_| queue == Queue::Reproduction)
                    .and_then(|open| open.opening.clone())
                {
                    return Ok(Action::Decide(opening));
                }
                let (task, tested) = match (queue, reason) {
                    (Queue::Reproduction, Reason::Reproduction { attempt, .. }) => (
                        TaskId {
                            queue: JournalQueue::Reproduction,
                            ordinal: open.as_ref().map_or(0, |open| open.ordinal),
                        },
                        match attempt {
                            AttemptScope::Exact => Tested::Exact {},
                            AttemptScope::Ccd { ccd } => Tested::Isolated {
                                ccd,
                                offset: profile.get(ccd).ok_or(NotReady::UnmatchedCell)?,
                            },
                        },
                    ),
                    (Queue::Mapping, Reason::Mapping { target }) => (
                        mapping_work
                            .as_ref()
                            .map(|(_, task)| *task)
                            .ok_or(NotReady::UnmatchedCell)?,
                        Tested::Isolated {
                            ccd: target.ccd,
                            offset: target.offset,
                        },
                    ),
                    (Queue::Qualification, _) => (
                        qualification
                            .as_ref()
                            .map(|(_, task)| *task)
                            .ok_or(NotReady::UnmatchedCell)?,
                        Tested::Exact {},
                    ),
                    (Queue::Requalification, _) => (
                        requalification
                            .as_ref()
                            .map(|(_, task)| *task)
                            .ok_or(NotReady::UnmatchedCell)?,
                        Tested::Exact {},
                    ),
                    (Queue::Controls, _) => (
                        TaskId {
                            queue: JournalQueue::Control,
                            ordinal: self.control_ordinal()?,
                        },
                        Tested::Exact {},
                    ),
                    (Queue::Reproduction | Queue::Mapping, _) => {
                        return Err(NotReady::UnmatchedCell);
                    }
                };
                let sig = Sig::of_cell(&cell);
                let retry = self.task_trials(task).any(|trial| {
                    trial.night() == Some(night)
                        && Sig::of_class(&self.class_of_intent(trial)) == sig
                });
                Chosen {
                    task,
                    activity: activity_of(queue),
                    cell,
                    profile,
                    tested,
                    retry,
                }
            }
            NightTask::StockUnstable { class } => return Ok(Action::Decide(self.dead_end(&class))),
            // A lapsed reproduction is closed before scheduling; with nothing due the filler runs.
            NightTask::Unreproduced { .. } | NightTask::Idle => {
                match self.filler(layout, mapping, night, unavailable)? {
                    Some(chosen) => chosen,
                    None => return Ok(self.end_unavailable(night, excluded)),
                }
            }
        };
        // Leaving a qualification task without completing it journals its progress.
        let previous = self
            .trials
            .by_night
            .get(&night)
            .and_then(|trials| trials.last())
            .and_then(|&at| self.trials.all.get(at))
            .and_then(super::books::Trial::task);
        if let Some(previous) = previous
            && previous != chosen.task
            && matches!(
                previous.queue,
                JournalQueue::Qualification | JournalQueue::Requalification
            )
            && let Some(owed) = self.qualification_commit_owed(true)?
        {
            return Ok(Action::Decide(owed));
        }
        if let Some(owed) = self.plan_owed(night, Some(chosen.task), false, excluded) {
            return Ok(Action::Decide(owed));
        }
        Ok(Action::RunTrial(TrialPlan {
            night,
            task: chosen.task,
            activity: chosen.activity,
            cell: chosen.cell,
            profile: chosen.profile,
            tested: chosen.tested,
            retry: chosen.retry,
            causes: Vec::new(),
        }))
    }

    /// The end of a night whose every runnable cell needs an excluded backend, once its plan records them.
    fn end_unavailable(&self, night: NightId, excluded: Vec<UnavailableBackend>) -> Action {
        match self.plan_owed(night, None, false, excluded) {
            Some(owed) => Action::Decide(owed),
            None => Action::EndNight(EndReason::Unavailable),
        }
    }

    /// The filler, run while no queue has due work. A cap probe first: on the CCD whose S is furthest from
    /// its observed or censored edge U (largest S − U, ties to the lowest CCD), the mapping matrix one count
    /// deeper than S, where that offset is admissible (at least U + `EDGE_MARGIN`). Otherwise D's exact
    /// qualification cells. The durable filler cursor, the count of filler trials before this one across
    /// every night, rotates the matrix and picks its cell round-robin, skipping cells that need an excluded
    /// backend. Neither form finishes a mapping step. `None` when every such cell needs an excluded backend.
    fn filler(
        &self,
        layout: &Layout<'_>,
        mapping: &MappingPlan,
        night: NightId,
        unavailable: &BTreeMap<togi_machine::Backend, Vec<Seq>>,
    ) -> Result<Option<Chosen>, NotReady> {
        let task = TaskId {
            queue: JournalQueue::Filler,
            ordinal: self.night_ordinal(night),
        };
        let cursor = self.trials.filler_cursor();
        let caps = self.committed_caps()?;
        let mut probe: Option<(CcdId, TargetOffset, i64)> = None;
        for ccd in self.ccds() {
            let Some(edge) = mapping.edges.get(&ccd).and_then(EdgeU::offset) else {
                continue;
            };
            let Some(cap) = caps.get(ccd) else {
                continue;
            };
            let Ok(deeper) = TargetOffset::new(cap.get() - 1) else {
                continue;
            };
            if deeper.get() < edge.get() + i64::from(EDGE_MARGIN) {
                continue;
            }
            let distance = cap.get() - edge.get();
            if probe.is_none_or(|(_, _, furthest)| distance > furthest) {
                probe = Some((ccd, deeper, distance));
            }
        }
        if let Some((ccd, offset, _)) = probe {
            let matrix = mapping_cells(layout, ccd, offset, cursor)?;
            if let Some(cell) = rotated(&matrix, cursor, unavailable) {
                return Ok(Some(Chosen {
                    task,
                    activity: Activity::Mapping,
                    cell,
                    profile: matrix.profile,
                    tested: Tested::Isolated { ccd, offset },
                    retry: false,
                }));
            }
        }
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        let matrix =
            qualification_cells(layout, &history.current().profile, cursor, &Reuse::AllFresh)?;
        Ok(rotated(&matrix, cursor, unavailable).map(|cell| Chosen {
            task,
            activity: Activity::Qualification,
            cell,
            profile: matrix.profile,
            tested: Tested::Exact {},
            retry: false,
        }))
    }

    /// The class of a recorded trial's intent, for telling a retry from a first attempt.
    fn class_of_intent(&self, trial: &super::books::Trial) -> togi_journal::ExposureClass {
        self.derive_exposure(trial)
            .map_or_else(Self::empty_class, |observed| observed.class)
    }

    fn empty_class() -> togi_journal::ExposureClass {
        togi_journal::ExposureClass {
            regime: togi_machine::Regime::R1,
            workloads: Vec::new(),
            cores: togi_machine::CoreSet::default(),
            position: None,
            duration_s: 0,
            placement: togi_journal::Placement::Exact,
        }
    }
}
