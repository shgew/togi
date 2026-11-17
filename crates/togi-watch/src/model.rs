//! The model: the journal folded into the facts one frame shows.
//!
//! [`Model::fold_batch`] takes each batch once, through the journal's and the policy's own states, retaining
//! outcomes only for its last trial end. The policy's projection is computed once per change, on the first
//! [`Model::snapshot`] after it, never per frame. The views add only what the journal says beside it: when, why
//! and after what. Raw facts stay raw; the frames escape them at output.

use std::cell::OnceCell;
use std::collections::BTreeSet;
use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{
    Activity, DeadEndCondition, Event, Payload, Seq, ShutdownReason, State as JournalState, TrialId,
};
use togi_machine::{
    CcdId, Condition, CoreId, CoreSet, ObservedOffset, PerCore, Regime, ResetKind, Signal,
    TargetOffset, WorkloadId,
};
use togi_tuner::policy::{Params, Projection as PolicyProjection, State as PolicyState};

use crate::derive::Derived;
use crate::facts::Facts;
use crate::history::{Entry, LOG_LIMIT, LogEntry};
use crate::screen::Log;
use crate::tuning::TuningClock;

/// Why the model cannot stand for the journal: one of its folds refused an event.
#[derive(Debug, Clone, thiserror::Error)]
pub enum FoldError {
    /// The journal's own state refused an event.
    #[error(transparent)]
    Journal(#[from] togi_journal::FoldError),
    /// The policy state refused an event.
    #[error(transparent)]
    Tuner(#[from] togi_tuner::policy::FoldError),
}

/// The first event a batch could not fold.
#[derive(Debug, thiserror::Error)]
#[error("cannot fold journal event at batch index {index}")]
pub struct BatchFoldError {
    /// The zero-based index of the refused event within the batch.
    pub index: usize,
    /// Why the model refused it.
    #[source]
    pub source: FoldError,
}

/// One core as a frame shows it.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CoreView {
    /// The core.
    pub id: CoreId,
    /// Its CCD.
    pub ccd: CcdId,
    /// The last hardware readback or restoration; `None` while the journal records none for the core, which is
    /// not the same as an offset of 0.
    pub applied: Option<ObservedOffset>,
    /// The day offset D of the core's CCD, CO 0 before any `day.applied`.
    pub profile: TargetOffset,
    /// Whether the trial in flight loads it.
    pub loaded: bool,
}

/// A trial as the journal recorded its intent.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TrialView {
    /// Its id.
    pub id: TrialId,
    /// Its regime.
    pub regime: Regime,
    /// Its workload.
    pub workload: WorkloadId,
    /// Cores under load.
    pub cores: CoreSet,
    /// Cores that are not loaded.
    pub idle: CoreSet,
    /// Its condition.
    pub condition: Condition,
    /// What the session was doing when it asked for it.
    pub activity: Option<Activity>,
    /// The night task it serves, once the policy plans one.
    pub task: Option<togi_journal::TaskId>,
    /// What it tests: one CCD alone at an offset, or the exact whole profile.
    pub tested: Option<togi_journal::Tested>,
    /// The offsets it runs at.
    pub profile: PerCore<TargetOffset>,
    /// When it started, once it has.
    pub started: Option<Timestamp>,
    /// A later boot has written events and this trial never recorded an end.
    pub crashed: bool,
    /// Its planned length.
    pub duration: Duration,
    /// A retry of a trial that could not run.
    pub retry: bool,
}

impl TrialView {
    /// How long a started trial is past its planned end without a recorded end, or zero. A crashed trial has no
    /// end to be past; less than a second past is not past.
    pub fn past_end(&self, now: Timestamp) -> Duration {
        let Some(started) = self.started else {
            return Duration::ZERO;
        };
        if self.crashed || self.duration.is_zero() {
            return Duration::ZERO;
        }
        let past =
            now.as_nanosecond() - started.as_nanosecond() - self.duration.as_nanos().cast_signed();
        let past = crate::numbers::nonnegative_elapsed(past);
        if past.as_secs() == 0 {
            Duration::ZERO
        } else {
            past
        }
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
    /// The stop restored the hardware's offsets; the profile itself is kept.
    pub saved: bool,
}

/// The session ended at a dead end.
#[derive(Debug, Clone, Eq, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot<'a> {
    /// The session, once started.
    pub session: Option<&'a SessionView>,
    /// Failure decisions, skips of trials that already failed included.
    pub failures: u32,
    /// Boots that ended without a clean shutdown.
    pub crashes: u32,
    /// The time spent inside runs, crash reboots included, the time between runs excluded.
    pub tuning: &'a TuningClock,
    /// The last failure that changed a decision; skips are excluded.
    pub last_failure: Option<Timestamp>,
    /// Every core, in core order.
    pub cores: &'a [CoreView],
    /// The trial in flight: its intent is recorded and it has not ended.
    pub trial: Option<&'a TrialView>,
    /// The last trial that ended.
    pub last: Option<&'a TrialEndView>,
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
    /// The unclean ends the day observer recorded and the answers given to them.
    pub day: &'a togi_journal::DayQuestions,
    /// Observed hours and crashes beside the CO 0 totals.
    pub day_totals: &'a crate::day_totals::DayTotals,
    /// The day steps a `day.applied` carried out. The policy projection holds the latest proposal, applied or not.
    pub applied_steps: &'a BTreeSet<togi_journal::DecisionId>,
    /// Policy facts, including unit-generic bounded exploration and retained diagnostic evidence.
    pub policy: &'a PolicyProjection,
    /// What the lenient read of the journal skipped or ignored, in one line; shown whatever else is on screen.
    pub skipped: Option<&'a str>,
    /// While the session rests between trials: what the trial that just ended led to, as it stood before its
    /// end. The outcome lines keep showing it until the next trial's intent is recorded.
    pub ended: Option<EndedTrial<'a>>,
}

/// The trial that just ended, with the policy as it stood before its end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndedTrial<'a> {
    /// The trial, before its end.
    pub trial: &'a TrialView,
    /// The policy as it stood then.
    pub policy: &'a PolicyProjection,
}

/// A started idle trial: nothing on the screen may change with the clock until its end is recorded, so drawing
/// never wakes the idle cores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleHold {
    /// When the trial started.
    pub started: Timestamp,
    /// When it is planned to end. The hold lasts past it, until the end is recorded.
    pub planned_end: Timestamp,
}

impl<'a> Snapshot<'a> {
    /// The started idle (R6) trial that holds every frame, or `None` when none runs, the session stopped, met a
    /// dead end, or the trial crashed.
    pub fn idle_hold(&self) -> Option<IdleHold> {
        let trial = self.trial?;
        let started = trial.started?;
        let live = self.session.is_some()
            && self.dead_end.is_none()
            && self.stopped.is_none()
            && !trial.crashed
            && trial.regime == Regime::R6
            && !trial.duration.is_zero();
        if !live {
            return None;
        }
        let planned_end = started
            .checked_add(jiff::SignedDuration::try_from(trial.duration).ok()?)
            .ok()?;
        Some(IdleHold {
            started,
            planned_end,
        })
    }

    /// Whether no trial is in flight to show, or the session has stopped, met a dead end or just recovered from a
    /// crash.
    pub fn resting(&self) -> bool {
        self.session.is_some()
            && (self.dead_end.is_some()
                || self.stopped.is_some()
                || self.recover.is_some()
                || self.crashed_trial()
                || self.trial.is_none())
    }

    /// Whether the session is live with no trial in flight: one ended, or none has run yet, and nothing has
    /// stopped it, ended it or crashed since. Every panel keeps its place then, as while a trial runs or is
    /// prepared.
    pub fn between_trials(&self) -> bool {
        self.session.is_some()
            && self.trial.is_none()
            && self.dead_end.is_none()
            && self.stopped.is_none()
            && self.recover.is_none()
    }

    /// Whether the journal's tail is a trial whose boot a later boot has outlived: the trial crashed, and the next
    /// run has yet to record it.
    pub fn crashed_trial(&self) -> bool {
        self.trial.is_some_and(|trial| trial.crashed)
    }

    /// The log as the log view lists it, with nothing arrived since.
    pub fn log(&self) -> Log<'a> {
        Log {
            entries: self.log,
            total: self.log_total,
            arrived: 0,
        }
    }

    /// Whether the day step the policy shows was carried out: a `day.applied` cites its decision. A hold applies
    /// nothing, so it is never proposed.
    pub fn step_applied(&self, step: &togi_journal::DayStep) -> bool {
        matches!(step.step, togi_journal::StepKind::Hold { .. })
            || self.applied_steps.contains(&step.decision)
    }
}

/// Presentation facts and the last trial end a reader can still show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presentation {
    facts: Facts,
    ended: Option<EndState>,
}

#[derive(Clone, Debug, PartialEq)]
enum EndState {
    Ready(Box<crate::derive::EndView>),
    Deferred(Seq),
}

fn clears_retained_end(event: &Event) -> bool {
    matches!(
        event.payload,
        Payload::SessionStart(_)
            | Payload::TrialIntent(_)
            | Payload::Shutdown(_)
            | Payload::CrashDetected(_)
    )
}

impl Presentation {
    /// The only end in a batch whose pre-end view can still be shown afterwards.
    pub fn batch_end(events: &[Event]) -> Option<Seq> {
        events
            .iter()
            .rev()
            .take_while(|event| !clears_retained_end(event))
            .find(|event| matches!(event.payload, Payload::TrialEnd(_)))
            .map(|event| event.seq)
    }

    fn retain_end(&mut self, event: &Event, end: impl FnOnce(&Facts) -> Option<EndState>) {
        if clears_retained_end(event) {
            self.ended = None;
        } else if matches!(event.payload, Payload::TrialEnd(_)) {
            self.ended = end(&self.facts);
        }
    }

    /// Captures the pre-end view when this end remains observable after the batch.
    pub fn before_event(
        &mut self,
        event: &Event,
        journal: &JournalState,
        policy: &PolicyState,
        keep: bool,
    ) {
        self.retain_end(event, |facts| {
            (keep && journal.session.is_some()).then(|| {
                EndState::Ready(Box::new(crate::derive::EndView::new(
                    journal, policy, facts,
                )))
            })
        });
    }

    /// The live session has no frame to show yet; remember the end to reconstruct if it returns live folds.
    pub fn defer_before_event(&mut self, event: &Event) {
        self.retain_end(event, |_| Some(EndState::Deferred(event.seq)));
    }

    /// The pre-end checkpoint the session still needs to reconstruct.
    pub fn deferred_end(&self) -> Option<Seq> {
        match self.ended {
            Some(EndState::Deferred(seq)) => Some(seq),
            Some(EndState::Ready(_)) | None => None,
        }
    }

    /// Replaces only the retained end, preserving the current presentation facts.
    pub fn materialize_end(&mut self, ready: Self) {
        self.ended = ready.ended;
    }

    /// Updates presentation facts after the shared reducers consume the event.
    pub fn after_event(&mut self, event: &Event, journal: &JournalState) {
        self.facts.fold(event, journal);
    }
}

/// The journal folded into facts.
#[derive(Debug)]
pub struct Model {
    journal: JournalState,
    policy: PolicyState,
    presentation: Presentation,
    problem: Option<FoldError>,
    skipped: Option<String>,
    derived: OnceCell<Derived>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            journal: JournalState::default(),
            policy: PolicyState::new(Params::SPEC),
            presentation: Presentation::default(),
            problem: None,
            skipped: None,
            derived: OnceCell::new(),
        }
    }
}

impl Model {
    /// The shared journal fold used by this model.
    pub const fn journal_state(&self) -> &JournalState {
        &self.journal
    }

    /// The incremental policy fold used by this model.
    pub const fn tuner_state(&self) -> &PolicyState {
        &self.policy
    }

    /// A model of a journal with no events.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets what the lenient read of the journal skipped or ignored, as the journal crate's report says it; `None`
    /// when it skipped nothing. The frames show it as long as it is set.
    pub fn set_skipped(&mut self, skipped: Option<String>) {
        self.skipped = skipped;
    }

    /// Folds the next event of the journal. A refusal turns the model into a problem state: the facts folded so
    /// far are dropped rather than shown half applied, and every later fold fails with the same error.
    pub fn fold(&mut self, event: &Event) -> Result<(), FoldError> {
        self.fold_ending(event, true)
    }

    /// Folds a batch in journal order, stopping at the first refused event. Only the last trial end retains its
    /// pre-end outcomes for the next frame; a batch without a trial end leaves the previous outcomes in place.
    /// An empty batch leaves the model unchanged.
    pub fn fold_batch(&mut self, events: &[Event]) -> Result<(), BatchFoldError> {
        let retained = Presentation::batch_end(events);
        for (at, event) in events.iter().enumerate() {
            self.fold_ending(event, retained == Some(event.seq))
                .map_err(|source| BatchFoldError { index: at, source })?;
        }
        Ok(())
    }

    fn fold_ending(&mut self, event: &Event, keep_outcomes: bool) -> Result<(), FoldError> {
        if let Some(problem) = &self.problem {
            return Err(problem.clone());
        }
        self.derived = OnceCell::new();
        self.presentation
            .before_event(event, &self.journal, &self.policy, keep_outcomes);
        let folded = self
            .journal
            .apply(event)
            .map_err(FoldError::from)
            .and_then(|()| self.policy.fold(event).map_err(FoldError::from));
        match folded {
            Ok(()) => {
                self.presentation.after_event(event, &self.journal);
                Ok(())
            }
            Err(error) => {
                self.presentation = Presentation::default();
                self.problem = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Why the model cannot stand for the journal, once a fold failed.
    pub fn problem(&self) -> Option<&FoldError> {
        self.problem.as_ref()
    }

    /// The facts a frame shows, borrowed from the model. The policy's projection is computed here
    /// the first time after a change and kept until the next fold.
    pub fn snapshot(&self) -> Snapshot<'_> {
        let derived = self.derived.get_or_init(|| {
            if self.problem.is_some() || self.journal.session.is_none() {
                Derived::empty()
            } else {
                Derived::new(&self.journal, &self.policy, &self.presentation.facts)
            }
        });
        let live = self.problem.is_none() && self.journal.session.is_some();
        let facts = &self.presentation.facts;
        let log = &facts.log[facts.log.len().saturating_sub(LOG_LIMIT)..];
        let mut snapshot = Snapshot {
            session: facts.session.as_ref().filter(|_| live),
            failures: facts.failures,
            crashes: facts.crashes,
            tuning: &facts.tuning,
            last_failure: facts.last_failure,
            cores: &derived.cores,
            trial: derived.trial.as_ref(),
            last: facts.last.as_ref(),
            recover: derived.recover.as_ref(),
            stopped: facts.stop.as_ref().filter(|_| facts.is_stopped()),
            dead_end: facts
                .dead_end
                .as_ref()
                .filter(|_| self.journal.dead_end.is_some()),
            history: &derived.history,
            history_dropped: derived.history_dropped,
            log,
            log_total: facts.log_total,
            day: &facts.day,
            day_totals: &facts.day_totals,
            policy: &derived.policy,
            applied_steps: &facts.applied_steps,
            skipped: self.skipped.as_deref(),
            ended: None,
        };
        if let Some(EndState::Ready(ended)) = &self.presentation.ended
            && let Some(trial) = ended.trial.as_ref()
        {
            snapshot.ended = (snapshot.between_trials()
                && snapshot.last.is_some_and(|last| last.id == trial.id))
            .then_some(EndedTrial {
                trial,
                policy: &ended.policy,
            });
        }
        snapshot
    }
}

/// Folds events into a new model, stopping at the first one the model refuses. The model is then in its problem
/// state, like one that was folded event by event.
pub fn project(events: &[Event]) -> Model {
    let mut model = Model::new();
    // A refused batch leaves the model in the problem state returned to the caller.
    let _ = model.fold_batch(events);
    model
}
