//! What the model computes once per change: the policy's projection, and the views that join it
//! with what the journal recorded.

use togi_journal::State as JournalState;
use togi_machine::{CoreSet, TargetOffset};
use togi_tuner::policy::{Params, Projection as PolicyProjection, State as PolicyState};

use crate::facts::{Facts, secs, trial_cores};
use crate::history::Entry;
use crate::model::{CoreView, RecoveryView, TrialView};

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Clone)]
pub(crate) struct Derived {
    pub(crate) view: View,
}

impl std::ops::Deref for Derived {
    type Target = View;

    fn deref(&self) -> &View {
        &self.view
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Clone)]
pub(crate) struct View {
    pub(crate) policy: PolicyProjection,
    pub(crate) cores: Vec<CoreView>,
    pub(crate) trial: Option<TrialView>,
    pub(crate) recover: Option<RecoveryView>,
    pub(crate) history: Vec<Entry>,
    pub(crate) history_dropped: usize,
}

impl Derived {
    pub(crate) fn empty() -> Self {
        Self {
            view: View::empty(),
        }
    }

    pub(crate) fn new(journal: &JournalState, policy: &PolicyState, facts: &Facts) -> Self {
        Self {
            view: View::new(journal, policy, facts),
        }
    }
}

/// The ended trial and the policy as it stood before its end.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Clone)]
pub(crate) struct EndView {
    pub(crate) trial: Option<TrialView>,
    pub(crate) policy: PolicyProjection,
}

impl EndView {
    pub(crate) fn new(journal: &JournalState, policy: &PolicyState, facts: &Facts) -> Self {
        let trial = facts.current.as_ref().map(|current| {
            let mut view = new_trial(&current.intent, facts, journal);
            view.crashed = current.boot_ended;
            view
        });
        Self {
            trial,
            policy: policy.project(),
        }
    }
}

impl View {
    fn empty() -> Self {
        Self {
            policy: PolicyState::new(Params::SPEC).project(),
            cores: Vec::new(),
            trial: None,
            recover: None,
            history: Vec::new(),
            history_dropped: 0,
        }
    }

    pub(crate) fn new(journal: &JournalState, policy: &PolicyState, facts: &Facts) -> Self {
        let policy = policy.project();
        let trial = facts.current.as_ref().map(|current| {
            let mut view = new_trial(&current.intent, facts, journal);
            view.crashed = current.boot_ended;
            view
        });
        let recover = facts.recover.as_ref().map(|recover| RecoveryView {
            crash_at: recover.crash_at,
            reset: recover.reset,
            trial: recover
                .intent
                .as_ref()
                .map(|intent| new_trial(intent, facts, journal)),
            end: recover.end.clone(),
        });
        let cores = core_views(journal, &policy, facts, trial.as_ref());
        let (history, history_dropped) = facts.history.recent();
        Self {
            policy,
            cores,
            trial,
            recover,
            history,
            history_dropped,
        }
    }
}

/// A trial as its intent recorded it; the cores it does not load are idle.
fn new_trial(
    intent: &togi_journal::TrialIntent,
    facts: &Facts,
    journal: &JournalState,
) -> TrialView {
    let cores = trial_cores(intent);
    let idle: CoreSet = journal
        .cores
        .iter()
        .map(|core| core.core)
        .filter(|core| !cores.contains(*core))
        .collect();
    TrialView {
        id: intent.trial.clone(),
        regime: intent.regime,
        workload: intent.workload,
        cores,
        idle,
        condition: intent.condition,
        activity: intent.activity,
        task: intent.task,
        tested: intent.tested,
        profile: intent.profile,
        started: facts.starts.get(&intent.trial).copied(),
        crashed: false,
        duration: secs(intent.duration_s),
        retry: intent.retry,
    }
}

/// Every core in core order, with its CCD's day offset and whether the trial in flight loads it.
fn core_views(
    journal: &JournalState,
    policy: &PolicyProjection,
    facts: &Facts,
    trial: Option<&TrialView>,
) -> Vec<CoreView> {
    let mut views: Vec<CoreView> = journal
        .cores
        .iter()
        .map(|core| {
            let profile = policy
                .units
                .iter()
                .find(|unit| unit.cores.contains(core.core))
                .map_or(TargetOffset::ZERO, |ccd| ccd.day);
            let loaded = trial
                .is_some_and(|trial| trial.started.is_some() && trial.cores.contains(core.core));
            CoreView {
                id: core.core,
                ccd: core.ccd,
                applied: facts.applied.get(core.core).flatten(),
                profile,
                loaded,
            }
        })
        .collect();
    views.sort_by_key(|view| view.id);
    views
}
