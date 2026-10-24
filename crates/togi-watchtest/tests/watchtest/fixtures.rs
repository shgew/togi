use std::collections::HashMap;

use togi_journal::{CycleEvent, Event, EventKind, Outcome, Payload, TrialId, TrialIntent};
use togi_machine::Signal;
use togi_tuner::State;
use togi_watchtest::{Base, cycle_completed_by_rerun};

use crate::support::{fold, intent_of};

#[test]
fn reruns_that_pass_complete_the_cycle_before_the_next_cycle_starts() {
    let journals = cycle_completed_by_rerun().expect("the fixture builds");
    assert!(!journals.running.is_empty());
    for events in &journals.running {
        let Some(Payload::TrialStart(start)) = events.last().map(|event| &event.payload) else {
            panic!("a running journal ends on trial.start");
        };
        assert!(intent_of(events, &start.trial).rerun, "a rerun runs");
        assert!(!events.iter().any(
            |event| matches!(&event.payload, Payload::TrialEnd(end) if end.trial == start.trial)
        ));
    }
    let Some(Payload::TrialEnd(end)) = journals.passed.last().map(|event| &event.payload) else {
        panic!("the passed journal ends on trial.end");
    };
    assert_eq!(end.outcome, Outcome::Pass);
    assert!(intent_of(&journals.passed, &end.trial).rerun);
    let Some(Payload::CheckingCycle(cycle)) = journals.ended.last().map(|event| &event.payload)
    else {
        panic!("the ended journal ends on the cycle's end");
    };
    assert_eq!((cycle.cycle.get(), cycle.event), (1, CycleEvent::End));
    assert!(journals.ended.starts_with(&journals.passed));
    assert!(
        journals
            .ended
            .iter()
            .filter_map(|event| {
                if let Payload::TrialIntent(intent) = &event.payload {
                    Some(intent)
                } else {
                    None
                }
            })
            .all(|intent| intent.cycle.is_none_or(|cycle| cycle.get() == 1)),
        "no cycle 2 trial has begun"
    );
    let r6_failed = journals.ended.iter().any(|event| {
        matches!(&event.payload, Payload::TrialEnd(end) if end.outcome == Outcome::Failure && end.core.is_some_and(|core| core.get() == 0))
    });
    assert!(r6_failed, "R6 failed naming core 00");
    fold(&journals.ended);
}

/// Of the cycle trials that ended in a crash, outside hunts, rounds and reruns: how many there are, and how many left
/// the cycle at another step than the one they ran in, by the time the next trial was asked for.
pub(crate) fn crashed_cycle_trials(events: &[Event]) -> (usize, usize) {
    let mut state = State::new();
    let mut intents: HashMap<TrialId, TrialIntent> = HashMap::new();
    let mut ran_in: HashMap<TrialId, usize> = HashMap::new();
    let mut pending: Vec<usize> = Vec::new();
    let (mut crashed, mut moved) = (0, 0);
    let mut settle = |state: &State, pending: &mut Vec<usize>| {
        let plan = state.cycle_plan();
        for ran in pending.drain(..) {
            crashed += 1;
            if plan.number.is_some() && plan.current + 1 != ran {
                moved += 1;
            }
        }
    };
    for event in events {
        if event.kind() == EventKind::TrialIntent {
            settle(&state, &mut pending);
        }
        state.fold(event).expect("the journal folds");
        match &event.payload {
            Payload::TrialIntent(intent) => {
                intents.insert(intent.trial.clone(), intent.clone());
            }
            Payload::TrialStart(start) => {
                let step = state.cycle_plan().current + 1;
                let intent = &intents[&start.trial];
                if let Some(recorded) = intent
                    .step
                    .filter(|_| intent.cycle.is_some() && !intent.rerun)
                {
                    assert_eq!(
                        usize::try_from(recorded.get()).expect("step fits"),
                        step,
                        "trial {} starts in the step its intent names",
                        start.trial.as_str()
                    );
                }
                ran_in.insert(start.trial.clone(), step);
            }
            Payload::TrialEnd(end) if end.signal == Some(Signal::Crash) => {
                let intent = &intents[&end.trial];
                let cycle_trial = intent.cycle.is_some()
                    && !intent.rerun
                    && intent.hunt.is_none()
                    && intent.round.is_none();
                if cycle_trial {
                    pending.push(ran_in[&end.trial]);
                }
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
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
            | Payload::CorePhase(_)
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
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }
    settle(&state, &mut pending);
    (crashed, moved)
}

#[test]
fn a_crashed_cycle_trial_of_the_crashed_cycle_session_leaves_its_cycle_at_another_step() {
    let events = Base::CrashedCycle.events().expect("the base simulates");
    let (crashed, moved) = crashed_cycle_trials(events);
    assert!(crashed > 0, "a checking cycle trial crashed");
    assert!(
        moved > 0,
        "the cycle moved on after a crash: {crashed} crashed, {moved} moved"
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::Shutdown)
    );
}
