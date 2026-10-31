//! Step cost: what one query costs does not grow with the evidence behind it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use togi_journal::{
    DayCheckpoint, DayCleanStop, LoadMix, Payload, Queue, Shutdown, ShutdownReason, Tested,
};
use togi_machine::BootId;
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{DayQuery, NightQuery, State};

use crate::fixture::{Ending, Journal, at, contents};

/// How many times each state is queried; the fastest query measures it, free of scheduling noise.
const ROUNDS: u32 = 30;

/// How much slower the late query may be than the early one.
const SLACK: u32 = 3;

fn query(state: &State, now: Timestamp) -> Duration {
    let contents = contents();
    let unavailable = BTreeMap::new();
    let query = NightQuery {
        now,
        contents: &contents,
        unavailable: &unavailable,
    };
    let started = Instant::now();
    state.next(&query).unwrap();
    started.elapsed()
}

/// The fastest query of each state, the two queried in turn so that both see the same load.
fn fastest(early: &(State, Timestamp), late: &(State, Timestamp)) -> (Duration, Duration) {
    let mut best = (Duration::MAX, Duration::MAX);
    for _ in 0..ROUNDS {
        best.0 = best.0.min(query(&early.0, early.1));
        best.1 = best.1.min(query(&late.0, late.1));
    }
    best
}

#[test]
fn a_qualification_query_late_in_its_matrix_costs_what_an_early_one_costs() {
    let mut journal = Journal::ready();
    let mut early: Option<(State, Timestamp)> = None;
    let mut late: Option<(State, Timestamp)> = None;
    let mut trials = 0_u32;
    for step in 0..3_500_i64 {
        let now = at(100 + step);
        let Ok(plan) = journal.until_end(now, &BTreeMap::new()) else {
            break;
        };
        match plan.task.queue {
            Queue::Qualification => {
                trials += 1;
                let snapshot = Some((journal.state.clone(), now));
                if early.is_none() {
                    early = snapshot;
                } else {
                    late = snapshot;
                }
            }
            Queue::Filler => break,
            Queue::Mapping | Queue::Control | Queue::Requalification | Queue::Reproduction => {}
        }
        journal.run(&plan, Ending::Pass);
    }
    let (Some(early), Some(late)) = (early, late) else {
        panic!("the night ran {trials} qualification trials");
    };
    let (early_cost, late_cost) = fastest(&early, &late);
    assert!(
        late_cost <= early_cost * SLACK,
        "after {trials} qualification trials a query took {late_cost:?}, against {early_cost:?} at the first"
    );
}

/// Runs `days` days from day `first`, each a cold boot whose day query settles, an hour's checkpoint and
/// a clean stop.
fn run_days(journal: &mut Journal, first: u32, days: u32) {
    for day in first..first + days {
        let boot = format!("day{day}");
        journal.boot_start(&boot);
        journal.settle_day(&DayQuery {
            boot: BootId::from(boot.clone()),
            invocation: Invocation::ColdBoot {
                pause: Pause::Running,
            },
            now: at(i64::try_from(journal.events.len()).unwrap() + 1),
        });
        journal.append(
            DayCheckpoint {
                applied_secs: 3_600,
                load_mix: LoadMix::default(),
                watchdog: None,
            }
            .into(),
            &[],
        );
        journal.append(DayCleanStop {}.into(), &[]);
    }
}

/// The state at the first trial of the night after `days` days, with a time just after its last event and
/// the intervals the days committed.
fn first_trial_after(days: u32) -> ((State, Timestamp), usize) {
    let mut journal = Journal::ready();
    run_days(&mut journal, 1, days);
    journal.reboot("tuning");
    let now = at(i64::try_from(journal.events.len()).unwrap() + 1);
    journal.until_trial(now);
    let intervals = journal
        .events
        .iter()
        .filter(|event| matches!(event.payload, Payload::DayInterval(_)))
        .count();
    ((journal.state, now), intervals)
}

#[test]
fn a_night_query_after_many_days_costs_what_one_after_a_few_costs() {
    let (early, _) = first_trial_after(10);
    let (late, intervals) = first_trial_after(2_000);
    let (early_cost, late_cost) = fastest(&early, &late);
    assert!(
        late_cost <= early_cost * SLACK,
        "after {intervals} committed intervals a query took {late_cost:?}, against {early_cost:?} after ten days"
    );
}

/// The state at the filler trial of night `nights`: the first night ran to its filler, and each later one
/// was stopped by the operator and reopened straight into its filler.
fn filler_state_of_night(nights: u32) -> ((State, Timestamp), u32) {
    let (mut journal, _) = Journal::promoted_compact();
    let _ = journal.until_filler(|_| Ending::Pass);
    for _ in 1..nights {
        journal.append(
            Shutdown {
                reason: ShutdownReason::Signal,
                kernel_cursor: None,
                kernel_error: None,
                cycles: None,
            }
            .into(),
            &[],
        );
        let now = at(i64::try_from(journal.events.len()).unwrap() + 1);
        let plan = journal.until_trial(now);
        assert_eq!(
            plan.task.queue,
            Queue::Filler,
            "the night reopens at its filler"
        );
    }
    let now = at(i64::try_from(journal.events.len()).unwrap() + 1);
    journal.until_trial(now);
    ((journal.state, now), nights)
}

#[test]
fn a_filler_query_in_a_late_night_costs_what_one_in_an_early_night_costs() {
    let (early, _) = filler_state_of_night(3);
    let (late, nights) = filler_state_of_night(50_000);
    let (early_cost, late_cost) = fastest(&early, &late);
    assert!(
        late_cost <= early_cost * SLACK,
        "in night {nights} a filler query took {late_cost:?}, against {early_cost:?} in the third"
    );
}

/// The state at the filler trial after `fillers` filler trials of a night whose mapping failed at -10 and
/// deeper, where no deeper pass ever warns of those failures.
fn filler_state_after(fillers: u32) -> (State, Timestamp) {
    let (mut journal, _) = Journal::promoted_compact();
    let _ = journal.until_filler(|plan| {
        if matches!(plan.tested, Tested::Isolated { offset, .. } if offset.get() <= -10) {
            Ending::Fail(None)
        } else {
            Ending::Pass
        }
    });
    for _ in 0..fillers {
        let plan = journal.until_trial(journal.now());
        journal.run(&plan, Ending::Pass);
    }
    let now = journal.now();
    journal.until_trial(now);
    (journal.state, now)
}

#[test]
fn a_filler_query_late_in_a_long_filler_costs_what_the_first_one_costs() {
    let early = filler_state_after(1);
    let late = filler_state_after(10_000);
    let (early_cost, late_cost) = fastest(&early, &late);
    assert!(
        late_cost <= early_cost * SLACK,
        "after 10000 filler trials a query took {late_cost:?}, against {early_cost:?} at the first"
    );
}
