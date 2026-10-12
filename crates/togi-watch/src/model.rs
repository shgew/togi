//! The model: the journal folded into the facts one frame shows.
//!
//! [`Model::fold`] takes each event once, through the journal's and the tuner's own states. The tuner's
//! projection and forecast are computed once per change, on the first [`Model::snapshot`] after it, never per
//! frame. The views keep the tuner's plan types and add only what the journal says about them: when, why and
//! after what. Raw facts stay raw; the frames escape them at output.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{
    Activity, DeadEndCondition, Event, Member, Seq, ShutdownReason, State as JournalState, TrialId,
};
use togi_machine::{
    CcdId, Condition, CoreId, CoreSet, ObservedOffset, PerCore, Regime, ResetKind, Signal,
    TargetOffset, WorkloadId,
};
use togi_tuner::{
    BiosProfile, CombinationId, CyclePlan, DeepeningView, Forecast, HuntPlan, Limit, PhasePlan,
    R7CoreStatus, SearchTurn, State as TunerState, StepNo,
};

use crate::derive::Derived;
use crate::facts::Facts;
use crate::history::{Entry, LOG_LIMIT, LogEntry};
use crate::screen::Log;

/// Why the model cannot stand for the journal: one of the two folds refused an event.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FoldError {
    /// The journal's own state refused an event.
    #[error("{0}")]
    Journal(#[from] togi_journal::FoldError),
    /// The tuner's state refused an event.
    #[error("{0}")]
    Tuner(#[from] togi_tuner::FoldError),
}

/// Where a core stands on the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreState {
    /// At 0, waiting for its first search turn.
    Waiting,
    /// Searching.
    Search,
    /// Confirming a candidate solo limit.
    Confirm,
    /// Solo limit confirmed, waiting until every core has one.
    Found,
    /// At its limit.
    AtLimit,
    /// Has room to go deeper.
    HasRoom,
    /// A hunt candidate at its failing offset.
    Suspect,
    /// A member of a group the hunt keeps together, at its failing offset.
    Member,
    /// The member a probe moves shallower.
    Probe,
    /// Held shallower by a hunt.
    Parked,
}

/// The passes of a candidate solo limit being confirmed: light then heavy, each needing `needed` in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirm {
    /// The candidate offset.
    pub offset: TargetOffset,
    /// Light passes so far.
    pub light: u32,
    /// Heavy passes so far.
    pub heavy: u32,
    /// Passes each needs.
    pub needed: u32,
}

/// One core as a frame shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreView {
    /// The core.
    pub id: CoreId,
    /// Its CCD.
    pub ccd: CcdId,
    /// Where it stands.
    pub state: CoreState,
    /// The reset command that sent it back to search, until the search restarts.
    pub queued: Option<togi_journal::Seq>,
    /// The last hardware readback, distinct from the saved profile even after restoration.
    pub applied: ObservedOffset,
    /// The offset the tuner's profile holds for the core.
    pub profile: TargetOffset,
    /// Its solo limit, once confirmed or carried.
    pub solo: Option<TargetOffset>,
    /// The deepest offset that passed the search.
    pub pass: Option<TargetOffset>,
    /// The offset the search tries next.
    pub next: Option<TargetOffset>,
    /// Its failure point.
    pub fail: Option<TargetOffset>,
    /// What stops the core one count deeper than its profile offset, when anything does.
    pub holder: Option<Limit>,
    /// The combination whose backoff moved the core shallower than its solo limit.
    pub gave_back: Option<CombinationId>,
    /// Offsets at which a group trial of the running hunt failed with this core loaded.
    pub group_fails: Vec<TargetOffset>,
    /// The offset a parked core returns to when the hunt ends.
    pub returns_to: Option<TargetOffset>,
    /// The candidate solo limit being confirmed.
    pub confirm: Option<Confirm>,
    /// Whether the trial in flight loads it.
    pub loaded: bool,
    /// Whether the trial in flight can name it.
    pub judged: bool,
}

/// What a hunt sets each core to: its failing offsets and the offsets it parks the rest at.
#[derive(Debug, Clone, PartialEq)]
pub struct HuntShape {
    /// The failing profile.
    pub failing: PerCore<TargetOffset>,
    /// The profile that parks cores shallower.
    pub parked: PerCore<TargetOffset>,
}

/// A recorded combination: offsets at which these cores failed together.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboView {
    /// Its id.
    pub id: CombinationId,
    /// Its members, in core order.
    pub members: Vec<Member>,
    /// The hunt that found it.
    pub hunt: u32,
    /// Whether member probes ran; otherwise no smaller group was shown to fail.
    pub probed: bool,
    /// Whether it is an unresolved fallback.
    pub fallback: bool,
    /// The member it holds at its limit now, at its profile offset.
    pub holds: Option<Member>,
    /// Members shallower than their offset in the combination now.
    pub clear: CoreSet,
}

/// Where a checking trial stands in its cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CyclePlace {
    /// The cycle.
    pub cycle: u32,
    /// The step, counting from 1; `None` until the trial is placed.
    pub step: Option<u32>,
    /// The part of its step, counting from 1.
    pub part: Option<u32>,
    /// How many parts the step has.
    pub parts: u32,
}

/// Where a hunt trial stands in its hunt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HuntPlace {
    /// The hunt.
    pub hunt: u32,
    /// The group.
    pub group: u32,
    /// The part of the hunt's current split, counting from 1; `None` outside a split.
    pub part: Option<u32>,
    /// How many parts the current split has.
    pub parts: u32,
    /// The split's number among the hunt's splits, counting from 1; 0 when the plan is not a split.
    pub cut: u32,
    /// The member a probe moves shallower.
    pub probe: Option<Member>,
}

/// Which trial of its part or requirement a trial is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Count {
    /// Passes so far.
    pub passed: u32,
    /// The running trial, counting from 1.
    pub index: u32,
    /// How many are needed.
    pub of: u32,
}

/// A trial as the journal recorded its intent.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts of one trial"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct TrialView {
    /// Its id.
    pub id: TrialId,
    /// Its regime.
    pub regime: Regime,
    /// Its workload.
    pub workload: WorkloadId,
    /// Cores under load.
    pub cores: CoreSet,
    /// Cores a hunt holds shallower in this trial.
    pub parked: CoreSet,
    /// Cores neither loaded nor parked.
    pub idle: CoreSet,
    /// Its condition.
    pub condition: Condition,
    /// What the session was doing when it asked for it.
    pub activity: Option<Activity>,
    /// The offsets it runs at.
    pub profile: PerCore<TargetOffset>,
    /// When it started, once it has.
    pub started: Option<Timestamp>,
    /// A later boot has written events and this trial never recorded an end.
    pub crashed: bool,
    /// Its planned length.
    pub duration: Duration,
    /// An R7 trial that loads part of a CCD.
    pub partial: bool,
    /// A rerun of a failed trial.
    pub rerun: bool,
    /// A retry of a trial that could not run.
    pub retry: bool,
    /// Its place in a checking cycle.
    pub cycle: Option<CyclePlace>,
    /// Its place in a hunt.
    pub hunt: Option<HuntPlace>,
    /// Search and confirm: the core judged.
    pub core: Option<CoreId>,
    /// Search and confirm: the offset judged.
    pub offset: Option<TargetOffset>,
    /// The deepening round.
    pub round: Option<u32>,
    /// Its place in its requirement.
    pub count: Count,
}

impl TrialView {
    /// The all-zero rerun of a failing trial, run before a `failure_at_zero` dead end.
    pub fn zero_rerun(&self) -> bool {
        self.rerun && self.condition == Condition::Parked && self.hunt.is_none()
    }
}

/// How a trial ended.
#[derive(Debug, Clone, PartialEq)]
pub struct TrialEndView {
    /// The trial.
    pub id: TrialId,
    /// When its end was recorded.
    pub at: Timestamp,
    /// Its regime, when its intent is known.
    pub regime: Option<Regime>,
    /// The cores it loaded.
    pub cores: CoreSet,
    /// The outcome.
    pub outcome: togi_journal::Outcome,
    /// The signal of a failure.
    pub signal: Option<Signal>,
    /// The core it named.
    pub core: Option<CoreId>,
    /// How long it ran.
    pub duration: Duration,
    /// The length it was meant to run.
    pub planned: Option<Duration>,
    /// The last sample's time into the trial.
    pub last_sample: Option<Duration>,
    /// The core whose worker had stopped advancing by the last sample.
    pub stalled: Option<CoreId>,
    /// The highest Tctl.
    pub tctl_max_c: Option<i32>,
    /// The median voltage request.
    pub voltage_v: Option<f64>,
}

/// Why a hunt began.
#[derive(Debug, Clone, PartialEq)]
pub struct HuntCause {
    /// When the failure was recorded.
    pub at: Option<Timestamp>,
    /// The failing trial, when one ran.
    pub trial: Option<TrialView>,
    /// How the failing trial ended.
    pub end: Option<TrialEndView>,
    /// How it failed.
    pub signal: Option<Signal>,
    /// The core the failure named.
    pub core: Option<CoreId>,
    /// The failing profile already failed in a recorded trial; no trial ran.
    pub known: bool,
    /// With `known`: that trial was carried from an earlier session.
    pub carried: bool,
    /// With `known`: the regime of the skipped trial.
    pub regime: Option<Regime>,
    /// The failure came in a rerun after a backoff.
    pub rerun_of: bool,
    /// Which trial of its requirement failed, counting from 1.
    pub trial_number: u32,
}

/// What is recorded about a hunt beside the tuner's plan of it.
#[derive(Debug, Clone, PartialEq)]
pub struct HuntFacts {
    /// When it started.
    pub started: Option<Timestamp>,
    /// Why.
    pub cause: HuntCause,
    /// Parked cores run at offset 0.
    pub parked_zero: bool,
    /// How each group's trial failed, by group number.
    pub group_signals: Vec<(u32, Signal)>,
}

/// The running hunt: the tuner's plan and what the journal says about it.
#[derive(Debug, Clone, Copy)]
pub struct HuntView<'a> {
    /// The tuner's plan.
    pub plan: &'a HuntPlan,
    /// What the journal adds.
    pub facts: &'a HuntFacts,
}

impl HuntView<'_> {
    /// A located hunt still rerunning its failed load with every unloaded core at 0.
    pub fn locating(&self) -> bool {
        self.plan.groups.last().is_some_and(|group| {
            group.stage == togi_journal::HuntStage::Locate
                && group.outcome == togi_tuner::GroupState::Running
        })
    }

    /// How the failed trial of a group failed.
    pub fn group_signal(&self, group: u32) -> Option<Signal> {
        self.facts
            .group_signals
            .iter()
            .find(|(number, _)| *number == group)
            .map(|(_, signal)| *signal)
    }
}

/// The checking cycle: the tuner's plan and the hunts each step started.
#[derive(Debug, Clone, Copy)]
pub struct CycleView<'a> {
    /// The tuner's plan.
    pub plan: &'a CyclePlan,
    /// The hunts each step started, by step index.
    pub step_hunts: &'a [Vec<u32>],
    /// The step the tuner's next cycle trial runs in once reruns pass.
    pub resume: Option<StepNo>,
}

impl CycleView<'_> {
    /// The index of the step checking runs next, which skips the steps whose chain endings the tuner records
    /// without a trial; the first step not done when the tuner names none. It is the number of steps once every
    /// step is done.
    pub fn resume_step(&self) -> usize {
        match self.resume {
            Some(step) if (step.get().get() as usize) <= self.plan.steps.len() => {
                step.get().get() as usize - 1
            }
            _ => self.plan.current,
        }
    }
}

/// A crash detected on this boot, with no trial started since.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryView {
    /// When the crash was detected.
    pub crash_at: Timestamp,
    /// The reset the machine reported.
    pub reset: Option<ResetKind>,
    /// The trial the crash ended, when one was in flight.
    pub trial: Option<TrialView>,
    /// How that trial ended, once its end is recorded.
    pub end: Option<TrialEndView>,
}

/// The session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopView {
    /// When.
    pub at: Timestamp,
    /// Why.
    pub reason: ShutdownReason,
    /// The applied offsets were restored; the core rows show the saved profile.
    pub saved: bool,
}

/// The session ended at a dead end.
#[derive(Debug, Clone, PartialEq)]
pub struct DeadEndView {
    /// When.
    pub at: Timestamp,
    /// The condition.
    pub condition: DeadEndCondition,
    /// The recorded detail, raw.
    pub detail: String,
}

/// The session the journal records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionView {
    /// When it started.
    pub start: Timestamp,
    /// What it was last doing.
    pub activity: Option<Activity>,
}

/// Everything one frame shows, lent by the model. Empty when the journal has no session or the model is in a
/// problem state.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot<'a> {
    /// The session, once started.
    pub session: Option<&'a SessionView>,
    /// Failure decisions, skips of trials that already failed included.
    pub failures: u32,
    /// Boots that ended without a clean shutdown.
    pub crashes: u32,
    /// The last failure that changed a decision; skips are excluded.
    pub last_failure: Option<Timestamp>,
    /// Clean cycles counted for the current profile.
    pub clean_cycles: u32,
    /// Every core, in core order.
    pub cores: &'a [CoreView],
    /// The recorded combinations.
    pub combos: &'a [ComboView],
    /// Per R7 workload, CCD and core: whether the core has passed as a top requester.
    pub r7: &'a [R7CoreStatus],
    /// The trial in flight: its intent is recorded and it has not ended.
    pub trial: Option<&'a TrialView>,
    /// The last trial that ended.
    pub last: Option<&'a TrialEndView>,
    /// What each outcome of the trial in flight leads to, and what runs next between trials.
    pub forecast: &'a Forecast,
    /// The checking cycle.
    pub cycle: Option<CycleView<'a>>,
    /// The running hunt.
    pub hunt: Option<HuntView<'a>>,
    /// The search's turns, in the order the tuner takes them; empty once every core has a solo limit.
    pub turns: &'a [SearchTurn],
    /// Where the two-phase method stands.
    pub phases: Option<&'a PhasePlan>,
    /// The open deepening round.
    pub deepening: Option<&'a DeepeningView>,
    /// The profile to enter in BIOS.
    pub bios: Option<&'a BiosProfile>,
    /// A crash detected on this boot with no trial started since.
    pub recover: Option<&'a RecoveryView>,
    /// The session stopped.
    pub stopped: Option<&'a StopView>,
    /// The session ended at a dead end.
    pub dead_end: Option<&'a DeadEndView>,
    /// What happened, newest first, at most [`crate::history::HISTORY_LIMIT`].
    pub history: &'a [Entry],
    /// History entries older than the kept ones.
    pub history_dropped: usize,
    /// The journal's own lines, oldest first, at most [`crate::history::LOG_LIMIT`].
    pub log: &'a [LogEntry],
    /// Every event the log lists, kept or not.
    pub log_total: usize,
    /// Core ids in the session's order, ascending, the order every profile is listed in.
    pub order: &'a [CoreId],
    /// What each hunt sets each core to, by hunt number.
    pub shapes: &'a BTreeMap<u32, HuntShape>,
    /// The sequence numbers of trials carried from earlier sessions.
    pub carried: &'a BTreeSet<Seq>,
}

impl<'a> Snapshot<'a> {
    /// The log as the log view lists it, with nothing arrived since.
    pub fn log(&self) -> Log<'a> {
        Log {
            entries: self.log,
            total: self.log_total,
            arrived: 0,
        }
    }
}

/// The journal folded into facts.
#[derive(Debug, Default)]
pub struct Model {
    journal: JournalState,
    tuner: TunerState,
    facts: Facts,
    problem: Option<FoldError>,
    derived: OnceCell<Derived>,
}

impl Model {
    /// A model of a journal with no events.
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds the next event of the journal. A refusal turns the model into a problem state: the facts folded so
    /// far are dropped rather than shown half applied, and every later fold fails with the same error.
    pub fn fold(&mut self, event: &Event) -> Result<(), FoldError> {
        if let Some(problem) = &self.problem {
            return Err(problem.clone());
        }
        self.derived = OnceCell::new();
        let folded = self
            .journal
            .apply(event)
            .map_err(FoldError::from)
            .and_then(|()| self.tuner.fold(event).map_err(FoldError::from));
        match folded {
            Ok(()) => {
                self.facts.fold(event, &self.journal, &self.tuner);
                Ok(())
            }
            Err(error) => {
                self.facts = Facts::default();
                self.problem = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Why the model cannot stand for the journal, once a fold failed.
    pub fn problem(&self) -> Option<&FoldError> {
        self.problem.as_ref()
    }

    /// The facts a frame shows, borrowed from the model. The tuner's projection and forecast are computed here
    /// the first time after a change and kept until the next fold.
    pub fn snapshot(&self) -> Snapshot<'_> {
        let derived = self.derived.get_or_init(|| {
            if self.problem.is_some() || self.journal.session.is_none() {
                Derived::empty()
            } else {
                Derived::new(&self.journal, &self.tuner, &self.facts)
            }
        });
        let live = self.problem.is_none() && self.journal.session.is_some();
        let facts = &self.facts;
        let projection = &derived.projection;
        let plan = &projection.cycle_plan;
        let log = &facts.log[facts.log.len().saturating_sub(LOG_LIMIT)..];
        Snapshot {
            session: facts.session.as_ref().filter(|_| live),
            failures: facts.failures,
            crashes: facts.crashes,
            last_failure: facts.last_failure,
            clean_cycles: projection.clean_cycles,
            cores: &derived.cores,
            combos: &derived.combos,
            r7: &projection.r7_status,
            trial: derived.trial.as_ref(),
            last: facts.last.as_ref(),
            forecast: &derived.forecast,
            cycle: (plan.number.is_some() || !plan.steps.is_empty()).then(|| CycleView {
                plan,
                step_hunts: &derived.step_hunts,
                resume: derived.forecast.between.as_ref().and_then(|b| b.resume),
            }),
            hunt: projection
                .hunt_plan
                .as_ref()
                .zip(derived.hunt.as_ref())
                .map(|(plan, facts)| HuntView { plan, facts }),
            turns: &projection.search_turns,
            phases: projection.phase_plan.as_ref(),
            deepening: projection.deepening.as_ref(),
            bios: projection
                .bios_profile
                .as_ref()
                .filter(|bios| bios.confirmed.is_some()),
            recover: derived.recover.as_ref(),
            stopped: facts.stop.as_ref().filter(|_| facts.stopped),
            dead_end: facts
                .dead_end
                .as_ref()
                .filter(|_| self.journal.dead_end.is_some()),
            history: &derived.history,
            history_dropped: derived.history_dropped,
            log,
            log_total: facts.log_total,
            order: &derived.order,
            shapes: &facts.shapes,
            carried: &facts.carried,
        }
    }
}

/// Folds events into a new model, stopping at the first one the model refuses. The model is then in its problem
/// state, like one that was folded event by event.
pub fn project<'a>(events: impl IntoIterator<Item = &'a Event>) -> Model {
    let mut model = Model::new();
    for event in events {
        if model.fold(event).is_err() {
            break;
        }
    }
    model
}
