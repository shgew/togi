use togi_journal::{
    Activity, Combination, CycleEvent, Decision, HuntOutcome, HuntStage, Inference, Outcome,
};
use togi_machine::{Condition, Regime};

use super::support::{
    Log, cycle, decision, end, hunt_end, hunt_group, hunt_start, intent, nz, profile, rendered,
    seq, trial, trial_start,
};
use crate::hunts::{StepKey, compute_hunts, parked_label, render_hunts};
use crate::project::Projection;
use crate::report::compute;

fn commitment(result: HuntOutcome, combination: bool, cause: u64) -> Option<String> {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    log.add(hunt_start(1, "t", 1));
    log.add(hunt_end(1, result));
    if combination {
        log.add_caused(
            &[seq(3)],
            Combination {
                combination: nz(1),
                members: Vec::new(),
                fallback: false,
                hunt: nz(1),
                reason: String::new(),
            },
        );
    }
    log.add_caused(&[seq(cause)], decision(Decision::Backoff, 0, -10, -9));
    let journal = log.journal();
    Projection::new(&journal).hunts[0].commitment.clone()
}

#[test]
fn a_hunt_commits_only_to_a_backoff_that_cites_its_end_or_combination() {
    assert_eq!(commitment(HuntOutcome::Cancelled, false, 2), None);
    assert_eq!(commitment(HuntOutcome::Culprit, false, 2), None);
    assert_eq!(
        commitment(HuntOutcome::Culprit, false, 3).as_deref(),
        Some("00 -10->-9")
    );
    assert_eq!(
        commitment(HuntOutcome::Combination, true, 4).as_deref(),
        Some("00 -10->-9")
    );
}

#[test]
fn a_decision_other_than_a_backoff_is_no_commitment() {
    let mut log = Log::new(&[(0, 0)]);
    log.add(hunt_start(1, "t", 1));
    let end = log.add(hunt_end(1, HuntOutcome::Culprit));
    log.add_caused(&[end], decision(Decision::CheckSoloLimit, 0, -10, -9));
    let journal = log.journal();
    assert_eq!(Projection::new(&journal).hunts[0].commitment, None);
}

fn parked(offsets: &[i64], parked_seq: Option<u64>) -> String {
    let mut log = Log::new(&[(0, 0), (1, 0), (2, 1), (3, 1)]);
    let mut start = hunt_start(1, "0001", 1);
    start.parked = profile(offsets);
    start.parked_seq = parked_seq.map(seq);
    log.add(start);
    let journal = log.journal();
    parked_label(&Projection::new(&journal).hunts[0])
}

#[test]
fn a_hunt_names_how_its_offsets_were_parked() {
    assert_eq!(parked(&[0, 0], None), "all-zero");
    assert_eq!(parked(&[-10, 0], Some(7)), "#7");
    assert_eq!(parked(&[-30, -28, 0, 0], None), "loaded-held");
}

#[test]
fn an_open_hunt_runs_to_the_journals_last_event() {
    let mut log = Log::new(&[(0, 0)]);
    log.at(0).add(hunt_start(1, "t", 1));
    log.at(3600).add(hunt_start(2, "t", 1));
    log.at(7200).add(hunt_end(2, HuntOutcome::Cancelled));
    log.at(9000).add(cycle(1, CycleEvent::Start, false));
    let journal = log.journal();
    let p = Projection::new(&journal);
    let hours: Vec<_> = p
        .hunts
        .iter()
        .map(|h| h.end_time.duration_since(h.time).as_secs())
        .collect();
    assert_eq!(hours, [9000, 3600]);
    let text = rendered(|table| render_hunts(table, &compute_hunts(&p, None)));
    let rows: Vec<Vec<&str>> = text
        .lines()
        .skip(2)
        .map(|line| line.split_whitespace().collect())
        .collect();
    let columns = |row: &[&str]| (row[2].to_owned(), row[9].to_owned(), row[10].to_owned());
    assert_eq!(
        rows.iter().map(|row| columns(row)).collect::<Vec<_>>(),
        [
            ("2.500".to_owned(), "open".to_owned(), "-".to_owned()),
            ("1.000".to_owned(), "cancelled".to_owned(), "-".to_owned()),
        ],
        "{text}"
    );
}

#[test]
fn hunt_groups_are_counted_as_planned_run_inferred_and_skipped() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    log.add(hunt_start(1, "t", 1));
    let mut skipped = hunt_group(1, 1, HuntStage::Part);
    skipped.skipped = true;
    log.add(skipped);
    let mut inferred = hunt_group(1, 2, HuntStage::Part);
    inferred.inferred = Some(Inference::Failure);
    log.add(inferred);
    log.add(hunt_group(1, 3, HuntStage::Part));
    let mut ran = intent("g", Regime::R6, &[0, 1], &[0, 0]);
    ran.hunt = Some(nz(1));
    ran.group = Some(nz(3));
    ran.activity = Some(Activity::Hunt);
    ran.condition = Condition::Parked;
    trial(&mut log, ran, Outcome::Pass);
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    let h = &m.hunts[0];
    assert_eq!(
        (
            h.hunt.groups.len(),
            h.ran,
            h.inferred,
            h.skipped,
            h.hunt.trials.len()
        ),
        (3, 1, 1, 1, 1)
    );
}

#[test]
fn reruns_group_by_the_obligation_that_recorded_them() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    let first = trial(
        &mut log,
        intent("first", Regime::R6, &[0, 1], &[0, 0]),
        Outcome::Failure,
    );
    let second = trial(
        &mut log,
        intent("second", Regime::R6, &[0, 1], &[0, 0]),
        Outcome::Failure,
    );
    for (i, cause) in [first, first, first, first, first, first, second]
        .into_iter()
        .enumerate()
    {
        let id = i.to_string();
        let mut rerun = intent(&id, Regime::R6, &[0, 1], &[0, 0]);
        rerun.activity = Some(Activity::Checking);
        rerun.condition = Condition::Alone;
        rerun.rerun = true;
        log.add_caused(&[cause], rerun);
        log.add(trial_start(&id));
        log.add(end(
            &id,
            if i >= 5 {
                Outcome::Failure
            } else {
                Outcome::Pass
            },
        ));
    }
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!((m.checking.reruns, m.checking.reruns_failing), (2, 1));
    assert!(m.checking.steps.is_empty());
}

#[test]
fn checking_steps_list_reruns_apart_from_the_cycles() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    log.add(cycle(1, CycleEvent::Start, false));
    let step = |id: &str, cycle: u32, rerun: bool| {
        let mut planned = intent(id, Regime::R6, &[0, 1], &[-10, -10]);
        planned.activity = Some(Activity::Checking);
        planned.cycle = (!rerun).then(|| nz(cycle));
        planned.rerun = rerun;
        planned
    };
    trial(&mut log, step("pass", 1, false), Outcome::Pass);
    let failed = trial(&mut log, step("fail", 1, false), Outcome::Failure);
    let mut rerun = step("rerun", 1, true);
    rerun.cycle = None;
    log.add_caused(&[failed], rerun);
    log.add(trial_start("rerun"));
    log.add(end("rerun", Outcome::Pass));
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    let key = |rerun, cycle, outcome| StepKey {
        rerun,
        cycle,
        regime: Regime::R6,
        cores: "00,01".to_owned(),
        outcome: Some(outcome),
    };
    assert_eq!(
        m.checking.steps.into_iter().collect::<Vec<_>>(),
        [
            (key(false, 1, Outcome::Pass), 1),
            (key(false, 1, Outcome::Failure), 1),
            (key(true, 0, Outcome::Pass), 1),
        ]
    );
    assert_eq!((m.checking.cycles_started, m.checking.cycles_ended), (1, 0));
}
