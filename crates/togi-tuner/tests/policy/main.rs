//! The composed Ruleset 14 policy state, through its public API.

#![expect(
    clippy::unwrap_used,
    reason = "integration test fixtures and assertions panic on invalid evidence"
)]

mod day;
mod fixture;
mod flow;
mod night;
mod qualification;
mod step_cost;
mod unobserved;

use std::collections::BTreeMap;

use fixture::{Ending, Journal, at};
use togi_journal::{Activity, ClimbAction, ClimbDecision, Payload, SessionContext};
use togi_machine::{Backend, BootId, ContentId, Digest};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{Action, DayQuery, NightQuery, NotReady, Params, State};

#[test]
fn a_session_without_firmware_context_is_not_ready() {
    let journal = Journal::started();
    assert!(matches!(journal.next(at(0)), Err(NotReady::NoFirmware)));
}

#[test]
fn a_session_without_a_baseline_is_not_ready() {
    let journal = Journal::contextual();
    assert!(matches!(journal.next(at(0)), Err(NotReady::NoBaseline)));
}

fn next_plan(journal: &mut Journal) -> togi_tuner::policy::Decision {
    for _ in 0..50 {
        let action = journal.next(journal.now()).unwrap();
        let Action::Decide(decision) = action else {
            panic!("expected a plan before running a trial: {action:?}");
        };
        if matches!(
            Payload::from(decision.payload.clone()),
            Payload::NightPlan(_)
        ) {
            return decision;
        }
        journal.commit(Action::Decide(decision));
    }
    panic!("no night plan after the contextual commitments");
}

#[test]
fn the_first_query_of_a_night_opens_it_with_a_plan() {
    let mut journal = Journal::ready();
    let decision = next_plan(&mut journal);
    let Payload::NightPlan(plan) = Payload::from(decision.payload) else {
        panic!("expected a night.plan");
    };
    assert_eq!(plan.night, None);
    assert!(plan.tasks.is_empty());
    assert!(plan.budgets.iter().all(|budget| budget.consumed_s == 0));
}

#[test]
fn a_later_query_continues_the_open_night() {
    let mut journal = Journal::ready();
    let first = next_plan(&mut journal);
    let opened = journal.append(first.payload.into(), &first.causes);
    let later = Action::Decide(next_plan(&mut journal));
    let Action::Decide(decision) = later else {
        panic!("expected the task plan, got {later:?}");
    };
    let Payload::NightPlan(plan) = Payload::from(decision.payload) else {
        panic!("expected a night.plan");
    };
    assert_eq!(plan.night, Some(togi_journal::NightId(opened)));
}

#[test]
fn the_scheduler_starts_with_the_stock_controls() {
    let mut journal = Journal::ready();
    let plan = journal.next(at(100)).unwrap();
    journal.commit(plan);
    let mut action = journal.next(at(101)).unwrap();
    while let Action::Decide(_) = action {
        journal.commit(action);
        action = journal.next(at(101)).unwrap();
    }
    let Action::RunTrial(trial) = action else {
        panic!("expected a trial, got {action:?}");
    };
    assert_eq!(trial.activity, togi_journal::Activity::Control);
}

#[test]
fn a_restarted_state_asks_the_same_question() {
    let mut journal = Journal::ready();
    let plan = journal.next(at(100)).unwrap();
    journal.commit(plan);
    let mut resumed = State::new(Params::SPEC);
    for event in &journal.events {
        resumed.fold(event).unwrap();
    }
    let contents = togi_tuner::policy::Contents::new(std::iter::empty());
    let query = togi_tuner::policy::NightQuery {
        now: at(101),
        contents: &contents,
        unavailable: &BTreeMap::new(),
    };
    assert_eq!(
        format!("{:?}", journal.state.next(&query).unwrap()),
        format!("{:?}", resumed.next(&query).unwrap())
    );
}

#[test]
fn folding_the_journal_twice_changes_nothing() {
    let mut journal = Journal::ready();
    let plan = journal.next(at(100)).unwrap();
    journal.commit(plan);
    let before = serde_json::to_string(&journal.state.project()).unwrap();
    for event in &journal.events {
        journal.state.fold(event).unwrap();
    }
    assert_eq!(
        serde_json::to_string(&journal.state.project()).unwrap(),
        before
    );
}

/// Passes every trial the night asks for until one of `activity` is asked, returning the distinct
/// activities in the order they were first asked.
fn order_until(journal: &mut Journal, activity: Activity) -> Vec<Activity> {
    let mut seen: Vec<Activity> = Vec::new();
    for step in 0..1500_i64 {
        let plan = journal.until_trial(at(100 + step));
        if !seen.contains(&plan.activity) {
            seen.push(plan.activity);
        }
        if plan.activity == activity {
            return seen;
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("never asked for {activity:?}; saw {seen:?}");
}

#[test]
fn the_scheduler_orders_controls_then_mapping_then_qualification() {
    let mut journal = Journal::ready();
    assert_eq!(
        order_until(&mut journal, Activity::Qualification),
        [
            Activity::Control,
            Activity::Mapping,
            Activity::Qualification
        ]
    );
}

#[test]
fn an_interrupted_trial_is_observed_and_a_restart_continues_the_task() {
    let mut journal = Journal::ready();
    let plan = journal.until_trial(at(100));
    journal.run(&plan, Ending::Interrupted);
    let failure = journal.next(at(101)).unwrap();
    assert!(
        matches!(&failure, Action::Decide(d) if matches!(Payload::from(d.payload.clone()), Payload::Failure(_)))
    );
    journal.commit(failure);
    let exposure = journal.next(at(101)).unwrap();
    assert!(
        matches!(&exposure, Action::Decide(d) if matches!(Payload::from(d.payload.clone()), Payload::ExposureObserved(_)))
    );
    let mut resumed = State::new(Params::SPEC);
    for event in &journal.events {
        resumed.fold(event).unwrap();
    }
    let contents = fixture::contents();
    let query = NightQuery {
        now: at(101),
        contents: &contents,
        unavailable: &BTreeMap::new(),
    };
    assert_eq!(
        format!("{:?}", journal.state.next(&query).unwrap()),
        format!("{:?}", resumed.next(&query).unwrap())
    );
}

#[test]
fn a_firmware_change_puts_the_controls_back_in_front() {
    let mut journal = Journal::ready();
    order_until(&mut journal, Activity::Mapping);
    journal.append(
        SessionContext {
            bios_version: "2".into(),
            board: "board".into(),
            cpu_model: "cpu".into(),
            microcode: "mc".into(),
            boost_limit_mhz: 5000,
        }
        .into(),
        &[],
    );
    let plan = journal.until_trial(at(5_000));
    assert_eq!(plan.activity, Activity::Control);
}

#[test]
fn a_new_backend_build_puts_the_controls_back_in_front() {
    let mut journal = Journal::ready();
    order_until(&mut journal, Activity::Mapping);
    let plan = journal.until_trial(at(4_000));
    assert_eq!(plan.activity, Activity::Mapping);
    // The mapping trial ran a rebuilt mprime: the evidence context changed under it.
    journal.run_built(
        &plan,
        Ending::Pass,
        &ContentId {
            backend: Backend::Mprime,
            executable: Digest::from_bytes([9; 32]),
            configuration: Digest::from_bytes([2; 32]),
        },
    );
    let plan = journal.until_trial(at(5_000));
    assert_eq!(plan.activity, Activity::Control);
    assert!(plan.profile.is_zero());
}

#[test]
fn a_same_boot_day_query_at_zero_owes_nothing() {
    let mut journal = Journal::ready();
    let query = DayQuery {
        boot: BootId::from("boot".to_owned()),
        invocation: Invocation::SameBoot,
        now: at(100),
    };
    let owed = journal.settle_day(&query);
    assert!(
        flow::steps(&owed).is_empty(),
        "same-boot initialization never decides a step"
    );
    assert!(journal.state.day(&query).unwrap().is_empty());
}

#[test]
fn a_cold_boot_at_zero_holds_once() {
    let mut journal = Journal::ready();
    let query = DayQuery {
        boot: BootId::from("boot".to_owned()),
        invocation: Invocation::ColdBoot {
            pause: Pause::Running,
        },
        now: at(100),
    };
    let owed = journal.settle_day(&query);
    let decisions = flow::steps(&owed);
    let [step] = decisions.as_slice() else {
        panic!("expected one step, got {owed:?}");
    };
    assert!(matches!(
        step,
        ClimbDecision {
            decision: ClimbAction::Hold { .. },
            ..
        }
    ));
    assert!(journal.state.day(&query).unwrap().is_empty());
}

#[test]
fn a_serialized_state_resumes_as_the_same_state() {
    let mut journal = Journal::ready();
    for step in 0..40_i64 {
        let plan = journal.until_trial(at(100 + step));
        journal.run(&plan, Ending::Pass);
    }
    let saved = serde_json::to_string(&journal.state).unwrap();
    let restored: State = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored, journal.state);
    assert_eq!(
        serde_json::to_string(&restored.project()).unwrap(),
        serde_json::to_string(&journal.state.project()).unwrap()
    );
    let contents = fixture::contents();
    let query = NightQuery {
        now: at(200),
        contents: &contents,
        unavailable: &BTreeMap::new(),
    };
    assert_eq!(
        format!("{:?}", restored.next(&query).unwrap()),
        format!("{:?}", journal.state.next(&query).unwrap())
    );
}

#[test]
fn the_projection_names_the_units_the_day_and_the_running_task() {
    let mut journal = Journal::ready();
    let plan = journal.until_trial(at(100));
    journal.run(&plan, Ending::Pass);
    let projection = journal.state.project();
    let json = serde_json::to_value(&projection).unwrap();
    assert_eq!(
        json["units"],
        serde_json::json!([
            {"unit": {"ccd": 0}, "cores": json["units"][0]["cores"], "edge": null, "cap": null, "day": 0, "remaining": null},
            {"unit": {"ccd": 1}, "cores": json["units"][1]["cores"], "edge": null, "cap": null, "day": 0, "remaining": null},
        ])
    );
    assert_eq!(
        json["day"]["firmware"],
        serde_json::json!({"kind": "valid"})
    );
    assert_eq!(json["day"]["freeze"], serde_json::json!(false));
    let opened = json["night"]["night"].as_i64().unwrap();
    assert_eq!(json["night"]["started"], serde_json::json!(at(opened)));
    assert_eq!(json["night"]["closed_by"], serde_json::Value::Null);
    let restored: togi_tuner::policy::Projection = serde_json::from_value(json).unwrap();
    assert_eq!(restored, projection);
}

#[test]
fn the_incremental_state_equals_a_full_refold_and_a_resumed_capsule() {
    let mut journal = Journal::ready();
    let mut capsule: Option<(String, usize)> = None;
    for step in 0..120_i64 {
        if step == 60 {
            capsule = Some((
                serde_json::to_string(&journal.state).unwrap(),
                journal.events.len(),
            ));
        }
        let plan = journal.until_trial(at(100 + step));
        journal.run(&plan, Ending::Pass);
    }
    let mut refolded = State::new(Params::SPEC);
    for event in &journal.events {
        refolded.fold(event).unwrap();
    }
    assert_eq!(refolded, journal.state);
    let (saved, cut) = capsule.unwrap();
    let mut resumed: State = serde_json::from_str(&saved).unwrap();
    for event in &journal.events[cut..] {
        resumed.fold(event).unwrap();
    }
    assert_eq!(resumed, journal.state);
    let contents = fixture::contents();
    let query = NightQuery {
        now: at(400),
        contents: &contents,
        unavailable: &BTreeMap::new(),
    };
    let expected = format!("{:?}", journal.state.next(&query).unwrap());
    assert_eq!(format!("{:?}", refolded.next(&query).unwrap()), expected);
    assert_eq!(format!("{:?}", resumed.next(&query).unwrap()), expected);
    assert_eq!(resumed.project(), journal.state.project());
}
