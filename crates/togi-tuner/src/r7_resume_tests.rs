use togi_journal::Outcome;

use crate::testkit::{Journal, R7Report};

fn values(j: &Journal) -> Vec<i64> {
    j.offsets().iter().map(|(_, offset)| offset.get()).collect()
}

fn report() -> R7Report {
    R7Report::default()
        .requests(&[(0, 1.10), (1, 1.08)])
        .top(&[0])
}

fn scenario() -> Journal {
    let mut j = Journal::r7_session().resuming_every(1);
    for _ in 0..6 {
        j.r7_fact(true, &[0, 1], &[-30, -30, -30, -30], &report());
    }
    j.r7_trial(&[0, 1], &[-30, -30, -30, -30], Outcome::Pass, &report());
    let profile = values(&j);
    j.r7_failed(&[0, 1], &profile, &report());
    assert_eq!(j.drain_backoffs(8), [(0, -29)]);
    let profile = values(&j);
    j.r7_failed(
        &[0, 1, 2, 3],
        &profile,
        &R7Report::default()
            .named(2)
            .requests(&[(0, 1.1), (1, 1.08), (2, 1.09), (3, 1.07)]),
    );
    assert_eq!(j.drain_backoffs(8), [(2, -29)]);
    j.reset(1);
    j
}

#[test]
fn r7_crash_resume_fresh_equals_incremental() {
    let j = scenario();
    j.assert_resumes();
    let resumed = j.resumed();
    assert_eq!(resumed.state, j.state);
    assert_eq!(resumed.state.decide(), j.state.decide());
}

#[test]
fn repeated_r7_decide_is_equal_and_leaves_the_state_unchanged() {
    let mut j = scenario();
    let profile = values(&j);
    j.r7_failed(&[0, 1], &profile, &report());
    let before = j.state.clone();
    let first = j.state.decide();
    assert!(first.is_some());
    assert_eq!(first, j.state.decide());
    assert_eq!(first, j.state.clone().decide());
    assert_eq!(before, j.state);
}

fn decide_visits(passes: usize) -> (u64, usize) {
    let mut j = Journal::r7_session();
    for _ in 0..passes {
        j.r7_fact(true, &[0, 1], &[-30, -30, -30, -30], &report());
    }
    let profile = values(&j);
    j.r7_failed(&[0, 1], &profile, &report());
    j.state.visits();
    let _ = j.state.decide();
    (j.state.visits(), j.run().r7.targets.entries())
}

#[test]
fn r7_decide_visits_distinct_shapes_not_passes() {
    let small = decide_visits(50);
    let large = decide_visits(5_000);
    assert_eq!(small, large);
}
