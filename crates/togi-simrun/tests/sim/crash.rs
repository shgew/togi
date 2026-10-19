use std::cell::Cell;
use std::rc::Rc;

use jiff::SignedDuration;
use togi_journal::{DeadEndCondition, Event, EventKind, Outcome};
use togi_machine::{ResetKind, Signal};
use togi_session::Stop;
use togi_sim::definition::ScriptDef;

use crate::common::{Bench, finish, hunt_machine};
use togi_simrun::{Observe, Observer};

fn scripted_crash(reset: ResetKind) -> (Option<Stop>, Vec<Event>) {
    let mut definition = hunt_machine(4);
    definition.script = vec![ScriptDef {
        trial: "0001".into(),
        signal: Some(Signal::Crash),
        reset: Some(reset),
        ..ScriptDef::default()
    }];
    let bench = Bench::new(&definition, 1);
    let finished = finish(bench.input());
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    (finished.stop, bench.events())
}

fn reset_once_at(bench: &Bench, kind: EventKind, reset: ResetKind) -> (Rc<Cell<bool>>, Observer) {
    let fired = Rc::new(Cell::new(false));
    let flag = Rc::clone(&fired);
    let sim = bench.sim.clone();
    let observer = Box::new(move |event: &Event| {
        if !flag.get() && event.kind() == kind {
            flag.set(true);
            sim.next_reset(reset);
            sim.crash();
        }
        Observe::Continue
    });
    (fired, observer)
}

#[test]
fn a_power_button_reset_during_a_trial_is_a_conclusive_failure() {
    let (stop, events) = scripted_crash(ResetKind::PowerButton);
    let Some(Stop::DeadEnd { dead_end, .. }) = stop else {
        panic!("stop {stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
    let crash = find!(events, CrashDetected, |p| p.reset_reason
        == Some(ResetKind::PowerButton))
    .expect("no crash with the power-button reset reason");
    assert!(
        !crash.inconclusive,
        "power button during a trial was inconclusive"
    );
    assert!(
        find!(events, Failure, |p| p.signal == Signal::Crash).is_some(),
        "the power-button crash has no failure"
    );
}

#[test]
fn a_thermal_trip_reset_during_a_trial_dead_ends_without_a_failure() {
    let (stop, events) = scripted_crash(ResetKind::ThermalTrip);
    let Some(Stop::DeadEnd { dead_end, .. }) = stop else {
        panic!("stop {stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::ThermalTrip);
    let crash = find!(events, CrashDetected, |p| p.reset_reason
        == Some(ResetKind::ThermalTrip))
    .expect("no crash with the thermal-trip reset reason");
    assert!(
        crash.inconclusive,
        "thermal trip recorded as tuning failure"
    );
    assert!(find!(events, Failure).is_none());
}

#[test]
fn a_thermal_trip_while_a_trial_runs_dead_ends() {
    let bench = Bench::new(&hunt_machine(4), 1);
    let (fired, observer) = reset_once_at(&bench, EventKind::TrialStart, ResetKind::ThermalTrip);
    let mut input = bench.input();
    input.observer = Some(observer);
    let finished = finish(input);
    let events = bench.events();
    assert!(fired.get(), "the thermal trip never fired");
    let Some(Stop::DeadEnd { dead_end, .. }) = finished.stop else {
        panic!("stop {:?}", finished.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::ThermalTrip);
    let crash = find!(events, CrashDetected, |p| p.reset_reason
        == Some(ResetKind::ThermalTrip)
        && p.inconclusive)
    .expect("no inconclusive thermal-trip crash");
    assert!(
        crash.in_flight.is_some(),
        "the crash has no trial in flight"
    );
    assert!(
        find!(events, Failure).is_none(),
        "thermal trip recorded as tuning failure"
    );
}

#[test]
fn a_power_button_between_trials_is_inconclusive() {
    let bench = Bench::new(&hunt_machine(4), 1);
    let (fired, mut reset) = reset_once_at(&bench, EventKind::TrialEnd, ResetKind::PowerButton);
    let mut input = bench.input();
    input.observer = Some(Box::new(move |event| {
        reset(event);
        if let togi_journal::Payload::CrashDetected(crash) = &event.payload
            && crash.reset_reason == Some(ResetKind::PowerButton)
        {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    finish(input);
    let events = bench.events();
    assert!(fired.get(), "no between-trials power-button crash");
    let crash = find!(events, CrashDetected, |p| p.reset_reason
        == Some(ResetKind::PowerButton))
    .expect("no power-button crash recorded");
    assert!(
        crash.inconclusive,
        "the power-button crash between trials was conclusive"
    );
    assert!(
        find!(events, Failure).is_none(),
        "the power-button crash between trials recorded a tuning failure"
    );
}

#[test]
fn a_wall_clock_jump_does_not_lose_a_trials_corrected_mce() {
    let mut definition = hunt_machine(4);
    definition.script = vec![ScriptDef {
        trial: "0001".into(),
        signal: Some(Signal::CorrectedMce),
        at_s: 2.0,
        ..ScriptDef::default()
    }];
    let bench = Bench::new(&definition, 1);
    let jumped = Rc::new(Cell::new(false));
    let flag = Rc::clone(&jumped);
    let sim = bench.sim.clone();
    let mut input = bench.input();
    input.observer = Some(Box::new(move |event| {
        if event.kind() == EventKind::TrialStart && !flag.get() {
            flag.set(true);
            sim.jump_wall(SignedDuration::from_hours(-2)).unwrap();
            Observe::Continue
        } else if event.kind() == EventKind::TrialEnd {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    finish(input);
    let events = bench.events();
    assert!(
        jumped.get(),
        "the wall clock was not moved after trial start"
    );
    let intent =
        find!(events, TrialIntent, |p| p.trial.as_str() == "0001").expect("missing scripted trial");
    let end = find!(events, TrialEnd, |p| p.trial == intent.trial).expect("trial never ended");
    assert_eq!(end.outcome, Outcome::Failure);
    assert_eq!(end.signal, Some(Signal::CorrectedMce));
    assert!(
        find!(events, Mce, |p| p.corrected).is_some(),
        "the corrected machine check was lost after the wall clock jump"
    );
}
