use std::num::NonZeroU32;

use togi_journal::{Member, TrialId};
use togi_machine::{Condition, Regime};
use togi_tuner::{CyclePlan, GroupState, HuntPart, Lengths, Quota};

use super::hunt::{step, trial_view};
use super::kit::*;
use crate::derive::{Derived, fit_hunt, place_in_cycle};
use crate::model::{CoreState, Count, CyclePlace};

fn plan(steps: Vec<togi_tuner::CycleStep>, current: usize) -> CyclePlan {
    CyclePlan {
        number: NonZeroU32::new(1),
        open: true,
        steps,
        current,
        paused: false,
    }
}

fn placed(cores: &[u8], cycle: u32, step: Option<u32>) -> crate::model::TrialView {
    let mut trial = trial_view(cores, &[0], true, Condition::Together);
    trial.cycle = Some(CyclePlace {
        cycle,
        step,
        part: None,
        parts: 0,
    });
    trial
}

#[test]
fn a_cycle_trial_finds_its_part_in_the_plan() {
    let mut steps = vec![step(Regime::R1, true), step(Regime::R2, false)];
    steps[0].parts[0].cores = set(&[1]);
    steps[1].parts[0].running = true;
    steps[1].parts[0].passed = 1;
    steps[1].parts[0].lengths = Some(Lengths {
        short: Quota { count: 2, secs: 60 },
        long: Some(Quota {
            count: 1,
            secs: 600,
        }),
    });
    let cycle = plan(steps, 1);

    let mut running = placed(&[0], 1, None);
    place_in_cycle(&cycle, &mut running);
    let place = running.cycle.unwrap();
    assert_eq!((place.step, place.part, place.parts), (Some(2), Some(1), 1));
    assert_eq!(
        running.count,
        Count {
            passed: 1,
            index: 2,
            of: 3
        }
    );

    let mut other = placed(&[0], 2, None);
    place_in_cycle(&cycle, &mut other);
    assert_eq!(other.cycle.unwrap().step, None, "another cycle's trial");

    let mut before_it_runs = cycle.clone();
    before_it_runs.steps[1].parts[0].running = false;
    let mut waiting = placed(&[0], 1, None);
    place_in_cycle(&before_it_runs, &mut waiting);
    assert_eq!(
        waiting.cycle.unwrap().step,
        Some(2),
        "a trial no part runs yet is the current step's"
    );

    let mut named = placed(&[1], 1, Some(1));
    place_in_cycle(&before_it_runs, &mut named);
    assert_eq!(named.cycle.unwrap().step, Some(1));

    let mut absent = placed(&[0], 1, Some(9));
    place_in_cycle(&before_it_runs, &mut absent);
    assert_eq!(absent.cycle.unwrap().part, None, "a step the plan lacks");

    let mut elsewhere = placed(&[2], 1, None);
    place_in_cycle(&before_it_runs, &mut elsewhere);
    assert_eq!(
        elsewhere.cycle.unwrap().part,
        None,
        "no part has these cores"
    );

    let mut past_the_end = before_it_runs;
    past_the_end.current = 5;
    let mut late = placed(&[0], 1, None);
    place_in_cycle(&past_the_end, &mut late);
    assert_eq!(late.cycle.unwrap().step, None);
}

#[test]
fn the_now_count_credits_each_quota_not_every_pass() {
    let mut steps = vec![step(Regime::R2, false)];
    steps[0].parts[0].running = true;
    steps[0].parts[0].lengths = Some(Lengths {
        short: Quota { count: 3, secs: 60 },
        long: Some(Quota {
            count: 1,
            secs: 600,
        }),
    });
    steps[0].parts[0].passed = 3;
    let mut cycle = plan(steps, 0);
    let mut running = placed(&[0], 1, None);
    place_in_cycle(&cycle, &mut running);
    assert_eq!(
        running.count,
        Count {
            passed: 3,
            index: 4,
            of: 4
        },
        "three credited short passes while the long trial runs"
    );
    cycle.steps[0].parts[0].passed = 4;
    let mut done = placed(&[0], 1, None);
    place_in_cycle(&cycle, &mut done);
    assert_eq!(
        done.count,
        Count {
            passed: 4,
            index: 4,
            of: 4
        }
    );
}

#[test]
fn a_hunt_trial_takes_its_part_and_probe_from_the_plan() {
    let member = |core: u8| Member {
        core: self::core(core),
        offset: off(-19),
    };
    let mut hunt = super::hunt::plan(vec![super::hunt::group(
        4,
        &[0],
        &[-20, -30, -10],
        GroupState::Running,
        Some(member(2)),
        Vec::new(),
    )]);
    hunt.cut = 3;
    hunt.parts = vec![
        HuntPart {
            failing: set(&[0]),
            running: false,
            ..part_of()
        },
        HuntPart {
            failing: set(&[1]),
            running: true,
            ..part_of()
        },
    ];
    let mut trial = trial_view(&[0], &[-20], true, Condition::Parked);
    trial.hunt = Some(crate::model::HuntPlace {
        hunt: 1,
        group: 4,
        part: None,
        parts: 0,
        cut: 0,
        probe: None,
    });
    fit_hunt(&mut trial, &hunt);
    let place = trial.hunt.unwrap();
    assert_eq!((place.parts, place.cut, place.part), (2, 3, Some(2)));
    assert_eq!(place.probe, Some(member(2)));

    let mut other = trial_view(&[0], &[-20], true, Condition::Parked);
    other.hunt = Some(crate::model::HuntPlace {
        hunt: 2,
        parts: 0,
        ..place
    });
    fit_hunt(&mut other, &hunt);
    assert_eq!(
        other.hunt.unwrap().parts,
        0,
        "another hunt's trial is left as it was"
    );
}

#[test]
fn a_recovered_trial_takes_its_step_and_count_from_the_facts() {
    let mut h = Hist::three();
    let mut cycle = intent(
        "r1",
        Regime::R7,
        Condition::Together,
        &[-20, -30, -10],
        &[0, 1, 2],
    );
    cycle.cycle = Some(nz(1));
    let at = h.push(cycle.into());
    h.boot = "next".to_owned();
    h.push(crash("boot", Some(at)).into());
    let id = TrialId::from("r1".to_owned());

    let derived = Derived::new(&h.journal, &h.tuner, &h.facts);
    let place = derived
        .recover
        .as_ref()
        .unwrap()
        .trial
        .as_ref()
        .unwrap()
        .cycle
        .unwrap();
    assert_eq!(place.step, None, "no step was recorded");

    h.facts.steps.insert(id.clone(), 2);
    let count = Count {
        passed: 1,
        index: 2,
        of: 4,
    };
    h.facts.counts.insert(id, count);
    let derived = Derived::new(&h.journal, &h.tuner, &h.facts);
    let view = derived.recover.as_ref().unwrap().trial.as_ref().unwrap();
    assert_eq!(view.cycle.unwrap().step, Some(2));
    assert_eq!(view.count, count);
    assert!(
        derived.cores.is_empty(),
        "the tuner has no core the journal lists"
    );
}

fn part_of() -> HuntPart {
    HuntPart {
        failing: set(&[]),
        parked: set(&[]),
        trials: 0,
        duration_s: 0,
        group: None,
        outcome: None,
        running: false,
    }
}

fn limit_script(starts: &[Start]) -> Script {
    Script::new(&[(0, 0), (1, 0), (2, 1)], starts)
}

#[test]
fn cores_the_search_has_not_reached_wait_and_found_ones_say_so() {
    let script = limit_script(&[Start::search(-20), Start::solo(-20, -20), Start::search(0)]);
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(snap.cores[0].state, CoreState::Search);
    assert_eq!(snap.cores[1].state, CoreState::Found);
    assert_eq!(
        snap.cores[2].state,
        CoreState::Waiting,
        "a core at 0 with no pass yet"
    );
}

#[test]
fn a_core_confirming_its_solo_limit_shows_the_count() {
    let mut script = limit_script(&[
        Start::search(-20),
        Start::has_room(-20),
        Start::has_room(-20),
    ]);
    let mut check = transition(0, Start::search(-20));
    check.check_solo_limit = true;
    check.workloads = vec![
        togi_machine::workloads(Regime::R1)[0].id(),
        togi_machine::workloads(Regime::R2)[0].id(),
    ];
    script.append(check.into(), &[]);
    let model = script.model();
    let snap = model.snapshot();
    assert_eq!(snap.cores[0].state, CoreState::Confirm);
    let confirm = snap.cores[0].confirm.expect("the count");
    assert_eq!(confirm.offset, off(-20));
    assert!(confirm.needed > 0);
}

#[test]
fn a_combination_names_its_members_and_the_core_it_holds() {
    let mut script = limit_script(&[Start::has_room(-30); 3]);
    script.ranking();
    script.append(combination(1, 1, &[(0, -31), (2, -30)], false).into(), &[]);
    let model = script.model();
    let snap = model.snapshot();
    let combo = &snap.combos[0];
    assert_eq!(combo.id.get(), 1);
    assert_eq!(combo.hunt, 1);
    assert!(!combo.fallback && !combo.probed);
    assert_eq!(combo.members.len(), 2);
    assert_eq!(
        combo.clear,
        set(&[0]),
        "only core 0 sits shallower than its member offset"
    );
    assert_eq!(combo.holds.map(|m| m.core), Some(core(0)));
}

#[test]
fn a_recovered_trial_says_when_it_started_and_that_it_loaded_part_of_a_ccd() {
    let mut h = Hist::three();
    let part = intent(
        "p1",
        Regime::R7,
        Condition::Together,
        &[-20, -30, -10],
        &[0],
    );
    let at = h.push(part.into());
    let id = TrialId::from("p1".to_owned());
    h.push(trial_start(&id));
    h.boot = "next".to_owned();
    h.push(crash("boot", Some(at)).into());
    let derived = Derived::new(&h.journal, &h.tuner, &h.facts);
    let view = derived
        .recover
        .as_ref()
        .and_then(|recover| recover.trial.as_ref())
        .expect("the crashed trial");
    assert_eq!(view.id, id);
    assert!(
        view.started.is_some(),
        "the trial had started before the crash"
    );
    assert!(view.partial, "core 0 ran while core 1 of its CCD did not");
}
