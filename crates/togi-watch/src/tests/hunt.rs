use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{HuntStage, Member, Outcome};
use togi_machine::{
    CcdId, Condition, CoreSet, ObservedOffset, PerCore, Regime, Signal, TargetOffset, workloads,
};
use togi_tuner::{
    Action, CyclePart, CyclePlan, CycleStep, GroupState, HuntGroup, HuntPlan, Lengths, Quota,
    RerunPlan, StepNo,
};

use super::kit::*;
use crate::derive::{group_failures, hunt_facts, trial_role};
use crate::facts::Facts;
use crate::model::{
    CoreEvidence, CoreState, CoreView, Count, CycleView, HuntShape, HuntView, TrialView,
};

fn core_view(id: u8) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId(id / 8),
        state: CoreState::Waiting,
        queued: None,
        applied: Some(ObservedOffset(0)),
        profile: off(0),
        solo: None,
        pass: None,
        next: None,
        fail: None,
        holder: None,
        gave_back: None,
        group_fails: Vec::new(),
        returns_to: None,
        confirm: None,
        loaded: false,
        judged: false,
        evidence: CoreEvidence::default(),
    }
}

pub(super) fn trial_view(
    loaded: &[u8],
    profile: &[i64],
    started: bool,
    condition: Condition,
) -> TrialView {
    TrialView {
        id: togi_journal::TrialId::from("probe".to_owned()),
        regime: Regime::R7,
        workload: workloads(Regime::R7)[0].id(),
        cores: set(loaded),
        parked: CoreSet::default(),
        idle: CoreSet::default(),
        condition,
        activity: None,
        profile: offsets(profile),
        started: started.then_some(Timestamp::UNIX_EPOCH),
        crashed: false,
        duration: Duration::from_secs(120),
        partial: false,
        rerun: false,
        retry: false,
        cycle: None,
        hunt: None,
        core: None,
        offset: None,
        round: None,
        count: Count::default(),
    }
}

pub(super) fn group(
    number: u32,
    cores: &[u8],
    profile: &[i64],
    outcome: GroupState,
    probe: Option<Member>,
    held: Vec<Member>,
) -> HuntGroup {
    HuntGroup {
        number,
        cores: set(cores),
        profile: offsets(profile),
        stage: if probe.is_some() {
            HuntStage::Probe
        } else {
            HuntStage::Full
        },
        probe,
        held,
        outcome,
        passed: 0,
        needed: 5,
        carried: false,
        running: outcome == GroupState::Running,
    }
}

pub(super) fn plan(groups: Vec<HuntGroup>) -> HuntPlan {
    HuntPlan {
        number: 1,
        regime: Regime::R7,
        failure: seq(1),
        trial: None,
        candidates: set(&[0, 2]),
        cut: 0,
        parts: Vec::new(),
        groups,
        probes: Vec::new(),
        rerun: RerunPlan {
            regime: Regime::R7,
            cores: CoreSet::default(),
            lengths: Lengths {
                short: Quota {
                    count: 1,
                    secs: 600,
                },
                long: None,
            },
        },
    }
}

fn member(id: u8, offset: i64) -> Member {
    Member {
        core: core(id),
        offset: off(offset),
    }
}

#[test]
fn member_probe_rows() {
    struct Case {
        name: &'static str,
        loaded: &'static [u8],
        profile: [i64; 3],
        started: bool,
        held_only: bool,
        group_at_parked: bool,
    }
    let case = |name, loaded, profile, started, held_only, group_at_parked| Case {
        name,
        loaded,
        profile,
        started,
        held_only,
        group_at_parked,
    };
    let cases = [
        case(
            "all cores loaded",
            &[0, 1, 2][..],
            [-15, -30, -8],
            true,
            false,
            false,
        ),
        case(
            "loaded noncandidate at nonparked offset",
            &[0, 1, 2],
            [-15, -29, -8],
            true,
            false,
            false,
        ),
        case(
            "probe loaded alone",
            &[0],
            [-15, -30, -8],
            true,
            false,
            false,
        ),
        case(
            "held member loaded alone",
            &[2],
            [-15, -30, -8],
            true,
            false,
            false,
        ),
        case(
            "idle members judged while nonmember loaded",
            &[1],
            [-15, -30, -8],
            true,
            false,
            false,
        ),
        case(
            "held member outside group cores",
            &[0, 1, 2],
            [-15, -30, -8],
            true,
            true,
            false,
        ),
        case(
            "group member failing offset equals parked",
            &[0, 1, 2],
            [-15, -30, -10],
            true,
            false,
            true,
        ),
        case(
            "probe at parked offset",
            &[0, 1, 2],
            [-10, -30, -8],
            true,
            false,
            false,
        ),
        case(
            "held member at parked offset",
            &[0, 1, 2],
            [-15, -30, -5],
            true,
            false,
            false,
        ),
        case(
            "preparing member probe",
            &[0, 1, 2],
            [-15, -30, -8],
            false,
            false,
            false,
        ),
    ];
    for case in cases {
        let mut parked = [-10i64, -30, -5];
        if case.group_at_parked {
            parked[2] = -10;
        }
        let group_cores: &[u8] = if case.held_only { &[] } else { &[2] };
        let held = if case.group_at_parked {
            Vec::new()
        } else {
            vec![member(2, case.profile[2])]
        };
        let hunt = plan(vec![
            group(
                1,
                &[0, 2],
                &[-20, -30, -10],
                GroupState::Failed,
                None,
                Vec::new(),
            ),
            group(
                2,
                group_cores,
                &case.profile,
                GroupState::Running,
                Some(member(0, case.profile[0])),
                held,
            ),
        ]);
        let mut facts = Facts::default();
        facts.shapes.insert(
            1,
            HuntShape {
                failing: offsets(&[-20, -30, -10]),
                parked: offsets(&parked),
            },
        );
        let mut trial = trial_view(case.loaded, &case.profile, case.started, Condition::Parked);
        for id in 0..3u8 {
            let mut view = core_view(id);
            trial_role(&mut view, &mut trial, Some(&hunt), &facts);
            let loaded = case.loaded.contains(&id);
            let member = id == 0 || id == 2;
            let nonparked = case.profile[usize::from(id)] != parked[usize::from(id)];
            assert_eq!(
                view.loaded,
                case.started && loaded,
                "{} core {id} loaded",
                case.name
            );
            assert_eq!(
                view.judged,
                case.started && (member || loaded && nonparked),
                "{} core {id} judged",
                case.name
            );
        }
    }
}

#[test]
fn a_failed_probe_marks_the_probed_member() {
    let hunt = plan(vec![
        group(
            1,
            &[0, 2],
            &[-20, -30, -10],
            GroupState::Failed,
            None,
            Vec::new(),
        ),
        group(
            2,
            &[2],
            &[-15, -30, -10],
            GroupState::Failed,
            Some(member(0, -15)),
            vec![member(2, -10)],
        ),
        group(
            3,
            &[0],
            &[-20, -30, -10],
            GroupState::Failed,
            None,
            Vec::new(),
        ),
        group(4, &[1], &[-20], GroupState::Failed, None, Vec::new()),
    ]);
    assert_eq!(group_failures(&hunt, core(0)), [off(-20), off(-15)]);
    assert_eq!(group_failures(&hunt, core(1)), []);
    assert_eq!(group_failures(&hunt, core(2)), [off(-10)]);
}

fn hunt_cause(fact: togi_journal::Payload) -> crate::model::HuntCause {
    let mut h = Hist::three();
    let at = h.push(fact);
    let mut hunt = plan(Vec::new());
    hunt.failure = at;
    let empty = CyclePlan {
        number: None,
        open: false,
        steps: Vec::new(),
        current: 0,
        paused: false,
    };
    hunt_facts(&hunt, &empty, &h.facts, &h.journal).cause
}

#[test]
fn a_hunt_caused_by_a_carried_trial() {
    let cause = hunt_cause(carried_trial().into());
    assert!(cause.known && cause.carried);
    assert_eq!(cause.regime, Some(Regime::R7));
    assert!(cause.trial.is_none(), "no trial ran");
    assert_eq!(
        cause.signal,
        Some(Signal::ComputationError),
        "the carried payload records the signal"
    );
}

#[test]
fn a_hunt_caused_by_a_carried_idle_failure() {
    let cause = hunt_cause(carried_failure().into());
    assert!(cause.known && cause.carried);
    assert_eq!(cause.regime, None, "an idle crash is not a trial");
    assert_eq!(cause.core, Some(core(1)));
    assert_eq!(cause.signal, Some(Signal::Crash));
}

/// Runs the tuner against failing together trials until a parked group trial of a hunt is running.
fn hunting() -> (Script, togi_journal::TrialId, togi_journal::Seq) {
    let mut script = Script::at_limit();
    for _ in 0..200 {
        if script.shadow.hunt_plan().is_some() {
            if let Ok(Action::RunTrial(plan)) = script.shadow.next() {
                if plan.condition == Condition::Parked {
                    let (intent, trial) = script.intent(&plan);
                    script.start(&trial);
                    return (script, trial, intent);
                }
            }
        }
        script.step(|plan| {
            if plan.condition == Condition::Together {
                Outcome::Failure
            } else {
                Outcome::Pass
            }
        });
    }
    panic!("no parked group trial ran");
}

#[test]
fn a_parked_hunt_trial_shows_its_roles() {
    let (script, trial, _) = hunting();
    let model = script.model();
    let snap = model.snapshot();
    let hunt = snap.hunt.expect("the hunt");
    let running = snap.trial.expect("the running trial");
    assert_eq!(running.id, trial);
    let place = running.hunt.expect("a hunt trial");
    assert_eq!(place.hunt, hunt.plan.number);
    assert_eq!(place.parts, u32::try_from(hunt.plan.parts.len()).unwrap());
    let group = hunt
        .plan
        .groups
        .iter()
        .find(|g| g.outcome == GroupState::Running)
        .expect("a running group");
    assert!(group.cores.is_empty(), "a locate group has no members");
    assert!(
        hunt.locating(),
        "a hunt starts by rerunning its failed load with the other cores at 0"
    );
    let roles: Vec<_> = snap
        .cores
        .iter()
        .map(|c| (c.id.get(), c.loaded, c.judged, c.state))
        .collect();
    assert_eq!(
        roles,
        [
            (0, true, false, CoreState::AtLimit),
            (1, true, false, CoreState::AtLimit),
            (2, false, false, CoreState::Parked),
        ],
        "the loaded cores hold their failing offsets, the candidate is parked at 0, and no member is judged"
    );
    assert_eq!(snap.cores[2].returns_to, Some(snap.cores[2].profile));
    let cause = &hunt.facts.cause;
    assert_eq!(cause.signal, Some(Signal::ComputationError));
    assert!(cause.trial.is_some() && cause.end.is_some());
    assert!(hunt.facts.started.is_some());
    assert!(
        !snap.forecast.branches.is_empty(),
        "outcomes are forecast while a trial runs"
    );
}

#[test]
fn the_projection_and_forecast_are_computed_once_per_change() {
    let (mut script, _, _) = hunting();
    let model = script.model();
    let (first, second) = (model.snapshot(), model.snapshot());
    assert!(std::ptr::eq(first.forecast, second.forecast));
    assert!(std::ptr::eq(first.cores.as_ptr(), second.cores.as_ptr()));
    let _ = &mut script;
}

#[test]
fn zero_rerun_is_the_all_zero_rerun_of_a_failing_trial() {
    let mut trial = trial_view(&[0, 1, 2], &[0, 0, 0], true, Condition::Parked);
    trial.rerun = true;
    assert!(trial.zero_rerun());
    trial.condition = Condition::Together;
    assert!(!trial.zero_rerun(), "a rerun with the profile applied");
    trial.condition = Condition::Parked;
    trial.hunt = Some(crate::model::HuntPlace {
        hunt: 1,
        group: 1,
        part: None,
        parts: 0,
        cut: 0,
        probe: None,
    });
    assert!(!trial.zero_rerun(), "a hunt trial");
}

pub(super) fn step(regime: Regime, done: bool) -> CycleStep {
    CycleStep {
        regime,
        workload: workloads(regime)[0].id(),
        parts: vec![CyclePart {
            cores: set(&[0]),
            ccd: None,
            full: true,
            partial: false,
            lengths: Some(Lengths {
                short: Quota { count: 1, secs: 60 },
                long: None,
            }),
            passed: 0,
            failed: 0,
            done,
            running: false,
        }],
        chains_complete: true,
        done,
    }
}

#[test]
fn resume_step_skips_steps_the_tuner_records_without_a_trial() {
    let plan = CyclePlan {
        number: std::num::NonZeroU32::new(1),
        open: true,
        steps: vec![
            step(Regime::R1, true),
            step(Regime::R2, false),
            step(Regime::R7, false),
        ],
        current: 1,
        paused: false,
    };
    let hunts = vec![Vec::new(); 3];
    let view = |resume| CycleView {
        plan: &plan,
        step_hunts: &hunts,
        resume,
    };
    let at = |n: u32| Some(StepNo::new(nz(n)));
    assert_eq!(view(None).resume_step(), 1, "the first step not done");
    assert_eq!(view(at(3)).resume_step(), 2);
    assert_eq!(view(at(9)).resume_step(), 1, "a step the plan lacks");
}

#[test]
fn a_located_hunt_reruns_with_every_unloaded_core_at_zero() {
    let mut locate = group(
        1,
        &[0, 1, 2],
        &[0, 0, 0],
        GroupState::Running,
        None,
        Vec::new(),
    );
    locate.stage = HuntStage::Locate;
    let hunt = plan(vec![locate.clone()]);
    let facts = crate::model::HuntFacts {
        started: None,
        cause: hunt_cause(carried_trial().into()),
        parking: None,
        group_signals: vec![(1, Signal::Stall)],
    };
    let view = HuntView {
        plan: &hunt,
        facts: &facts,
    };
    assert!(view.locating());
    assert_eq!(view.group_signal(1), Some(Signal::Stall));
    assert_eq!(view.group_signal(2), None);
    locate.outcome = GroupState::Passed;
    let done = plan(vec![locate]);
    assert!(
        !HuntView {
            plan: &done,
            facts: &facts
        }
        .locating()
    );
    assert!(
        !HuntView {
            plan: &plan(Vec::new()),
            facts: &facts
        }
        .locating()
    );
}

#[test]
fn past_end_counts_whole_seconds_past_the_planned_end() {
    let mut trial = trial_view(&[0], &[0], true, Condition::Together);
    let end = Timestamp::UNIX_EPOCH
        .checked_add(jiff::SignedDuration::from_secs(120))
        .unwrap();
    let after = |secs| {
        end.checked_add(jiff::SignedDuration::from_secs(secs))
            .unwrap()
    };
    assert_eq!(trial.past_end(after(42)), Duration::from_secs(42));
    assert_eq!(trial.past_end(end), Duration::ZERO);
    assert_eq!(trial.past_end(after(-1)), Duration::ZERO);
    trial.crashed = true;
    assert_eq!(
        trial.past_end(after(42)),
        Duration::ZERO,
        "a crashed trial has no end to be past"
    );
    trial.crashed = false;
    trial.started = None;
    assert_eq!(trial.past_end(after(42)), Duration::ZERO);
    trial.started = Some(Timestamp::UNIX_EPOCH);
    trial.duration = Duration::ZERO;
    assert_eq!(trial.past_end(after(42)), Duration::ZERO);
}

#[test]
fn a_started_idle_trial_holds_every_frame_until_its_end_is_recorded() {
    let mut script = Script::at_limit();
    let mut idle = None;
    for _ in 0..200 {
        if let Ok(Action::RunTrial(plan)) = script.shadow.next() {
            if plan.regime == Regime::R6 {
                idle = Some(plan);
                break;
            }
        }
        script.step(|_| Outcome::Pass);
    }
    let plan = idle.expect("the cycle reaches its idle step");
    let (intent, trial) = script.intent(&plan);
    let model = script.model();
    assert!(
        model.snapshot().idle_hold().is_none(),
        "a trial intent does not freeze before the trial starts"
    );
    let started = script.start(&trial);
    let model = script.model();
    let hold = model
        .snapshot()
        .idle_hold()
        .expect("a started idle trial holds");
    assert_eq!(hold.started, at(started));
    assert_eq!(
        hold.planned_end,
        at(started)
            .checked_add(jiff::SignedDuration::from_secs(i64::from(
                plan.duration.get()
            )))
            .unwrap()
    );
    script.boot = "next".to_owned();
    script.append(warning("a later boot wrote this"), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert!(snap.crashed_trial(), "the trial's boot was outlived");
    assert!(snap.recover.is_none(), "no crash is recorded yet");
    assert!(
        snap.idle_hold().is_none(),
        "a later boot releases the hold before the crash is recorded"
    );
    script.append(crash("boot", Some(intent)).into(), &[]);
    let model = script.model();
    assert!(
        model.snapshot().idle_hold().is_none(),
        "a recorded crash releases the hold"
    );
    let _ = Duration::ZERO;
}

#[test]
fn r7_workload_and_top_requesters() {
    let mut script = Script::at_limit();
    script.step(|_| Outcome::Pass);
    script.step(|_| Outcome::Pass);
    let plan = script.next_trial();
    assert_eq!(plan.regime, Regime::R7);
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(
        snap.r7_workload(),
        Some(workloads(Regime::R7)[0].id()),
        "the cycle's current step names the workload before a trial runs"
    );
    let want: std::collections::BTreeMap<_, _> = snap
        .r7
        .iter()
        .filter(|s| s.workload == snap.r7_workload().unwrap() && s.top_requester)
        .map(|s| (s.core, s.offset_proxy))
        .collect();
    assert_eq!(snap.r7_top(), want);
    assert!(!want.is_empty(), "some core tops the request order");
    let _ = (TargetOffset::ZERO, PerCore::<TargetOffset>::default());
}

#[test]
fn resting_when_no_trial_runs() {
    let mut script = Script::at_limit();
    assert!(script.model().snapshot().resting(), "between trials");
    let plan = script.next_trial();
    let (_, trial) = script.intent(&plan);
    script.start(&trial);
    assert!(!script.model().snapshot().resting(), "a trial runs");
}

#[test]
fn a_hunt_is_filed_under_the_cycle_step_that_started_it() {
    let mut h = Hist::three();
    let mut plan = intent(
        "c1",
        Regime::R7,
        Condition::Together,
        &[-20, -30, -10],
        &[0, 1, 2],
    );
    plan.cycle = Some(nz(1));
    h.push(plan.into());
    let id = togi_journal::TrialId::from("c1".to_owned());
    h.facts.steps.insert(id.clone(), 2);
    let mut start = hunt_start(7, &[0, 2]);
    start.trial = Some(id);
    h.push(start.into());
    assert_eq!(h.facts.check_hunts.get(&(1, 2)), Some(&vec![7]));
}

#[test]
fn a_trial_outside_the_hunt_is_left_alone() {
    let hunt = plan(Vec::new());
    let mut trial = trial_view(&[0], &[-20], true, Condition::Together);
    crate::derive::fit_hunt(&mut trial, &hunt);
    assert!(trial.hunt.is_none());
    let mut core = core_view(0);
    trial_role(&mut core, &mut trial, Some(&hunt), &Facts::default());
    assert!(
        core.loaded && core.judged,
        "only a parked trial has roles of its own"
    );
    assert_eq!(core.state, CoreState::Waiting);
}

/// Eight cores on two CCDs failing together, until a hunt group with members runs parked.
fn hunting_members() -> (Script, togi_journal::TrialId) {
    let cores: Vec<(u8, u8)> = (0..8).map(|id| (id, id / 4)).collect();
    let mut script = Script::new(&cores, &[Start::at_limit(-30, -31); 8]);
    script.append(
        togi_journal::ProfileChange {
            from: None,
            to: (0..8).map(|id| (core(id), off(-30))).collect(),
        }
        .into(),
        &[],
    );
    script.ranking();
    for _ in 0..300 {
        if let (Ok(Action::RunTrial(plan)), Some(hunt)) =
            (script.shadow.next(), script.shadow.hunt_plan())
        {
            let members = hunt.groups.iter().any(|g| {
                g.outcome == GroupState::Running
                    && !g.cores.is_empty()
                    && g.stage != HuntStage::Locate
            });
            if plan.condition == Condition::Parked && members {
                let (_, trial) = script.intent(&plan);
                script.start(&trial);
                return (script, trial);
            }
        }
        script.step(|plan| {
            if plan.condition == Condition::Together {
                Outcome::Failure
            } else {
                Outcome::Pass
            }
        });
    }
    panic!("no hunt group with members ran");
}

#[test]
fn a_parked_group_judges_its_members_and_not_the_parked_cores() {
    let (script, trial) = hunting_members();
    let model = script.model();
    let snap = model.snapshot();
    let running = snap.trial.expect("the group trial");
    assert_eq!(running.id, trial);
    let hunt = snap.hunt.expect("the hunt");
    let group = hunt
        .plan
        .groups
        .iter()
        .find(|g| g.outcome == GroupState::Running)
        .expect("the running group");
    let mut members = 0;
    for core in snap.cores {
        if group.cores.contains(core.id) {
            members += 1;
            assert!(core.judged, "member {} is judged", core.id);
            assert!(
                matches!(core.state, CoreState::Member | CoreState::Probe),
                "member {}: {:?}",
                core.id,
                core.state
            );
        } else if running.parked.contains(core.id) {
            assert!(!core.judged, "parked core {} is not judged", core.id);
            if hunt.plan.candidates.contains(core.id) {
                assert_eq!(core.state, CoreState::Parked);
            }
        }
    }
    assert!(members > 0);
    assert!(
        snap.cores
            .iter()
            .any(|c| running.parked.contains(c.id) && !group.cores.contains(c.id)),
        "the group parks some cores"
    );
}

#[test]
fn a_hunt_parks_held_kept_shallow_and_zero_cores_apart() {
    let mut h = Hist::three();
    h.facts.shapes.insert(
        1,
        HuntShape {
            failing: offsets(&[-43, -45, -49, -49, -49, -49]),
            parked: offsets(&[-43, -45, -49, -49, -10, 0]),
        },
    );
    let mut hunt = plan(Vec::new());
    hunt.candidates = set(&[3, 4, 5]);
    let empty = CyclePlan {
        number: None,
        open: false,
        steps: Vec::new(),
        current: 0,
        paused: false,
    };
    let facts = hunt_facts(&hunt, &empty, &h.facts, &h.journal);
    let parking = facts
        .parking
        .as_ref()
        .expect("the hunt's shape is recorded");
    assert_eq!(parking.held, set(&[0, 1, 2]), "loaded cores at failing");
    assert_eq!(parking.kept, set(&[3]), "a candidate at its failing offset");
    assert_eq!(parking.zero, set(&[5]));
    assert_eq!(parking.shallow, [(core(4), off(-10))]);
    assert!(!facts.parked_at_zero(), "a kept core is not parked at 0");

    h.facts.shapes.insert(
        1,
        HuntShape {
            failing: offsets(&[-43, -45, -49, -49]),
            parked: offsets(&[-43, -45, 0, 0]),
        },
    );
    hunt.candidates = set(&[2, 3]);
    let located = hunt_facts(&hunt, &empty, &h.facts, &h.journal);
    assert!(located.parked_at_zero());
    assert_eq!(located.parking.expect("shape").held, set(&[0, 1]));
}
