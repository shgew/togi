//! Completed stock controls stay completed: a later full-CCD R7 load that reshapes the partial chain never
//! reopens them, while a stock failure, a pending CO-0 rerun and a new evidence context still act.

use std::collections::BTreeMap;

use test_case::test_case;
use togi_journal::{Queue, Tested};
use togi_machine::{CoreId, PerCore};
use togi_tuner::night::{PackageLoad, Roles};
use togi_tuner::policy::{EndReason, Params, State, TrialPlan};

use crate::fixture::{Ending, Journal, at, cores_per_ccd, rebuilt};

/// A night on two CCDs of four cores, so every CCD derives honest R7 partials.
fn eight_core_night() -> Journal {
    Journal::ready_on(Params::SPEC, cores_per_ccd(4))
}

/// What a full-CCD R7 load of the four cores 0–3 measures: core `i` requests `volts[i]`.
fn requests_of(volts: [f64; 4]) -> PerCore<f64> {
    (0..4_i64)
        .zip(volts)
        .map(|(id, volts)| (CoreId::new(id).unwrap(), volts))
        .collect()
}

fn is_full_ccd0(plan: &TrialPlan) -> bool {
    matches!(
        plan.cell.roles,
        Roles::Package(PackageLoad::FullCcd { ccd }) if ccd.get() == 0
    )
}

fn is_isolated_ccd0_mapping(plan: &TrialPlan) -> bool {
    plan.task.queue == Queue::Mapping
        && matches!(plan.tested, Tested::Isolated { ccd, offset }
            if ccd.get() == 0 && offset.get() < 0)
}

/// Drives the night with passing trials until its stock controls are done, and returns the first plan
/// beyond them, not yet run.
fn through_controls(journal: &mut Journal) -> TrialPlan {
    for _ in 0..2_000 {
        let plan = journal.until_trial(journal.now());
        if plan.task.queue != Queue::Control {
            return plan;
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("the stock controls never finished");
}

/// Runs `plan` and every later trial passing until CCD0's isolated mapping asks for its full-CCD load,
/// which measures the opposite order of the fallback chain: CCD0's partials change.
fn change_chain(journal: &mut Journal, mut plan: TrialPlan) {
    for _ in 0..2_000 {
        if is_isolated_ccd0_mapping(&plan) && is_full_ccd0(&plan) {
            journal.run_requesting(&plan, &requests_of([1.3, 1.2, 1.1, 1.0]));
            return;
        }
        journal.run(&plan, Ending::Pass);
        plan = journal.until_trial(journal.now());
    }
    panic!("mapping never loaded CCD0 in full");
}

/// The state a cold fold of the whole journal reaches, with no query before it.
fn refold(journal: &mut Journal) {
    let mut state = State::new(Params::SPEC);
    for event in &journal.events {
        state.fold(event).unwrap();
    }
    journal.state = state;
}

#[test_case(false; "incremental queries")]
#[test_case(true; "cold journal fold")]
fn a_chain_change_after_completed_controls_schedules_no_control(cold: bool) {
    let mut journal = eight_core_night();
    let first = through_controls(&mut journal);
    change_chain(&mut journal, first);
    if cold {
        refold(&mut journal);
    }
    for _ in 0..300 {
        let plan = journal.until_trial(journal.now());
        assert_ne!(plan.task.queue, Queue::Control, "{plan:?}");
        journal.run(&plan, Ending::Pass);
    }
}

#[test]
fn controls_unfinished_at_a_chain_change_keep_adapting_until_they_finish() {
    let mut journal = eight_core_night();
    let mut changed = false;
    let mut seen = Vec::new();
    for _ in 0..2_000 {
        let plan = journal.until_trial(journal.now());
        if plan.task.queue != Queue::Control {
            assert!(changed, "no control loaded CCD0 in full");
            return;
        }
        assert!(
            !seen.contains(&(plan.profile.clone(), plan.cell)),
            "controls repeated a cell: {plan:?}"
        );
        seen.push((plan.profile.clone(), plan.cell));
        if !changed && is_full_ccd0(&plan) {
            journal.run_requesting(&plan, &requests_of([1.3, 1.2, 1.1, 1.0]));
            changed = true;
        } else {
            journal.run(&plan, Ending::Pass);
        }
    }
    panic!("the stock controls never finished");
}

#[test_case(false; "incremental queries")]
#[test_case(true; "cold journal fold")]
fn a_stock_failure_after_completed_controls_still_stops_the_night(cold: bool) {
    let mut journal = eight_core_night();
    let first = through_controls(&mut journal);
    change_chain(&mut journal, first);
    // A failure on core 4, a CO-0 core of CCD1, owes an all-zero rerun of its cell.
    let failed = loop {
        let plan = journal.until_trial(journal.now());
        assert_ne!(plan.task.queue, Queue::Control, "{plan:?}");
        if is_isolated_ccd0_mapping(&plan) {
            journal.run(&plan, Ending::Fail(Some(4)));
            break plan;
        }
        journal.run(&plan, Ending::Pass);
    };
    if cold {
        refold(&mut journal);
    }
    let Some(rerun) = (0..5_000_i64).find_map(|step| {
        let plan = journal.until_trial(at(20_000 + step));
        if plan.task.queue == Queue::Control {
            return Some(plan);
        }
        assert_ne!(plan.task.queue, Queue::Filler, "the night never reran it");
        journal.run(&plan, Ending::Pass);
        None
    }) else {
        panic!("the night never reran the failed cell");
    };
    assert!(rerun.profile.is_zero());
    assert_eq!(rerun.cell, failed.cell);
    journal.run(&rerun, Ending::Fail(None));
    assert_eq!(
        journal.until_end(at(30_000), &BTreeMap::new()).unwrap_err(),
        EndReason::DeadEnd
    );
}

#[test_case(false; "incremental queries")]
#[test_case(true; "cold journal fold")]
fn a_new_evidence_context_owes_fresh_controls(cold: bool) {
    let mut journal = eight_core_night();
    let first = through_controls(&mut journal);
    change_chain(&mut journal, first);
    journal.rebuild(rebuilt());
    if cold {
        refold(&mut journal);
    }
    let owed = (0..100_i64).any(|step| {
        let plan = journal.until_trial(at(20_000 + step));
        let control = plan.task.queue == Queue::Control;
        journal.run(&plan, Ending::Pass);
        control
    });
    assert!(owed, "a rebuilt backend owed no stock controls");
}
