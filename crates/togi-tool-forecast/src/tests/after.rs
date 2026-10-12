use togi_journal::{CycleEvent, Dimension, Event, Payload, Refusal};

use super::support::{
    A, B, build, config_loaded, crash, cycle, dead_end, events, extend, hunt_end, hunt_start, id,
    intent, observed, outcome, session, shutdown_cycles, start, start_with,
};
use crate::outcome::follow;
use crate::{Anchor, Error, Outcome, Session, Status, after};

use super::support::Counting;

fn before() -> Vec<Event> {
    let mut payloads = start(A, "abc1234");
    payloads.extend([crash(), intent("0001", Some(7))]);
    events(1, payloads)
}

fn anchor() -> Anchor {
    Session::from_events(before())
        .unwrap()
        .anchor_at(togi_journal::Seq::new(4).unwrap())
        .unwrap()
}

fn counted(sessions: &[Session]) -> Result<Outcome, Error> {
    Ok(follow::<Counting>(sessions, &anchor())?.outcome)
}

#[test]
fn nothing_after_the_anchor_is_a_censored_run_at_the_session_profile() {
    let sessions = [Session::from_events(before()).unwrap()];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::Censored, 0.0, 0, 0, &[-10, -20])
    );
}

#[test]
fn events_before_the_anchor_are_excluded() {
    let after = [
        crash(),
        crash(),
        intent("0002", Some(8)),
        intent("0003", Some(8)),
    ];
    let sessions = [extend(&before(), 5, after)];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::Censored, 4.0, 2, 1, &[-10, -20])
    );
}

#[test]
fn a_hunt_answered_only_by_carried_facts_runs_no_trial() {
    let sessions = [extend(&before(), 5, [hunt_start(1), hunt_end(1)])];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::Censored, 2.0, 0, 0, &[-10, -20])
    );
}

#[test]
fn a_hunt_counts_once_however_many_trials_it_ran() {
    let trials = [
        intent("0002", Some(8)),
        intent("0003", Some(8)),
        intent("0004", None),
    ];
    let sessions = [extend(&before(), 5, trials)];
    assert_eq!(counted(&sessions).unwrap().hunts, 1);
}

#[test]
fn the_same_hunt_number_in_another_session_is_another_hunt() {
    let first = extend(&before(), 5, [intent("0002", Some(7))]);
    let next = session(
        1,
        [start_with(B, build("abc1234")), intent("0001", Some(7))],
    );
    assert_eq!(counted(&[first, next]).unwrap().hunts, 2);
}

#[test]
fn a_run_concludes_at_a_cycles_shutdown() {
    let sessions = [extend(&before(), 5, [crash(), shutdown_cycles()])];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::Concluded, 2.0, 1, 0, &[-10, -20])
    );
}

#[test]
fn nothing_after_a_cycles_shutdown_counts() {
    let first = extend(
        &before(),
        5,
        [crash(), shutdown_cycles(), crash(), intent("0002", Some(8))],
    );
    let next = session(
        1,
        [
            start_with(B, build("")),
            super::support::baseline([-12, -22]),
            crash(),
            intent("0001", Some(7)),
            dead_end(),
        ],
    );
    assert_eq!(
        counted(&[first, next]).unwrap(),
        outcome(Status::Concluded, 2.0, 1, 0, &[-10, -20])
    );
}

#[test]
fn a_session_after_the_terminal_point_need_not_be_replayable() {
    let first = extend(&before(), 5, [shutdown_cycles()]);
    let mut stale = build("abc1234");
    stale.ruleset = 0;
    let next = session(1, [start_with(B, stale)]);
    assert!(!next.replayable());
    assert_eq!(
        counted(&[first, next]).unwrap(),
        outcome(Status::Concluded, 1.0, 0, 0, &[-10, -20])
    );
}

#[test]
fn a_dead_end_ends_the_run_before_a_later_conclusion() {
    let sessions = [extend(
        &before(),
        5,
        [crash(), dead_end(), crash(), shutdown_cycles()],
    )];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::DeadEnd, 2.0, 1, 0, &[-10, -20])
    );
}

#[test]
fn a_new_session_without_a_terminal_point_supplies_the_profile() {
    let next = session(
        1,
        [
            start_with(B, build("abc1234")),
            super::support::baseline([-12, -22]),
            crash(),
            intent("0001", Some(7)),
        ],
    );
    let sessions = [Session::from_events(before()).unwrap(), next];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::Censored, 0.0, 1, 1, &[-12, -22])
    );
}

#[test]
fn a_cycle_start_after_a_clean_cycle_is_the_terminal_point_and_is_not_counted() {
    let after = [
        cycle(1, CycleEvent::Start, false),
        cycle(1, CycleEvent::End, true),
        crash(),
        cycle(2, CycleEvent::Start, false),
        crash(),
        shutdown_cycles(),
    ];
    let sessions = [extend(&before(), 5, after)];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::Concluded, 4.0, 1, 0, &[-10, -20])
    );
}

#[test]
fn a_cycle_start_before_any_clean_cycle_is_no_terminal_point() {
    let after = [
        cycle(1, CycleEvent::Start, false),
        cycle(1, CycleEvent::End, false),
        cycle(2, CycleEvent::Start, false),
        crash(),
    ];
    let sessions = [extend(&before(), 5, after)];
    assert_eq!(
        counted(&sessions).unwrap(),
        outcome(Status::Censored, 4.0, 1, 0, &[-10, -20])
    );
}

#[test]
fn an_unreplayable_newest_session_cannot_project_a_profile() {
    let mut stale = build("abc1234");
    stale.ruleset = 0;
    let only = session(
        1,
        [start_with(A, stale), super::support::baseline([-10, -20])],
    );
    let anchor = only.anchor_at(togi_journal::Seq::new(2).unwrap()).unwrap();
    let error = after(&[only], &anchor).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "session {A} cannot be replayed: the journal's ruleset is 0; this build's is {}",
            togi_tuner::RULESET
        )
    );
    assert!(matches!(
        error,
        Error::NotReplayable {
            refusal: Refusal {
                dimension: Dimension::Ruleset,
                recorded: 0,
                current: togi_tuner::RULESET,
            },
            ..
        }
    ));
}

#[test]
fn a_newer_schema_with_this_builds_ruleset_is_refused_for_its_schema() {
    let mut newer = build("abc1234");
    newer.schema = togi_journal::SCHEMA + 1;
    let only = session(
        1,
        [start_with(A, newer), super::support::baseline([-10, -20])],
    );
    assert!(!only.replayable());
    let anchor = only.anchor_at(togi_journal::Seq::new(2).unwrap()).unwrap();
    let error = after(&[only], &anchor).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "session {A} cannot be replayed: the journal's schema is {}; this build's is {}",
            togi_journal::SCHEMA + 1,
            togi_journal::SCHEMA
        )
    );
    assert!(matches!(
        error,
        Error::NotReplayable {
            refusal: Refusal {
                dimension: Dimension::Schema,
                ..
            },
            ..
        }
    ));
}

#[test]
fn an_unreplayable_session_holding_a_cycle_start_is_not_concluded_by_it() {
    let mut stale = build("abc1234");
    stale.ruleset = 0;
    let mut payloads = vec![start_with(A, stale), super::support::baseline([-10, -20])];
    payloads.extend([cycle(1, CycleEvent::Start, false), crash()]);
    let only = session(1, payloads);
    let anchor = only.anchor_at(togi_journal::Seq::new(2).unwrap()).unwrap();
    assert!(matches!(
        follow::<Counting>(&[only], &anchor),
        Err(Error::NotReplayable { .. })
    ));
}

#[test]
fn a_journal_that_does_not_follow_itself_is_a_fold_error() {
    let mut all = before();
    all.extend(events(7, [crash()]));
    let skipping = Session::from_events(all).unwrap();
    let anchor = skipping
        .anchor_at(togi_journal::Seq::new(4).unwrap())
        .unwrap();
    let error = follow::<Counting>(&[skipping], &anchor).err().unwrap();
    assert!(
        matches!(&error, Error::JournalFold { session, .. } if *session == id(A)),
        "{error}"
    );
}

#[test]
fn a_journal_the_tuner_cannot_follow_is_a_fold_error() {
    let mut payloads = start(A, "abc1234");
    let Payload::TrialIntent(mut unloaded) = intent("0001", None) else {
        unreachable!()
    };
    unloaded.core = None;
    payloads.push(Payload::TrialIntent(unloaded));
    let only = session(1, payloads);
    let anchor = only.anchor_at(togi_journal::Seq::new(2).unwrap()).unwrap();
    let error = after(&[only], &anchor).unwrap_err();
    assert!(
        matches!(&error, Error::TunerFold { session, .. } if *session == id(A)),
        "{error}"
    );
}

#[test]
fn the_real_tuner_projects_the_baseline_before_a_checking_profile_exists() {
    let only = session(1, start(A, "abc1234"));
    let anchor = only.anchor_at(togi_journal::Seq::new(2).unwrap()).unwrap();
    let got = after(&[only], &anchor).unwrap();
    assert_eq!(got.profile, observed(&[-10, -20]));
    assert_eq!(got.depth, -30);
    assert_eq!(got.status, Status::Censored);
}

#[test]
fn the_real_tuner_ends_a_run_at_a_cycles_shutdown_and_counts_crashes() {
    let base = events(1, start(A, "abc1234"));
    let only = extend(
        &base,
        3,
        [config_loaded("abc1234"), crash(), shutdown_cycles()],
    );
    let anchor = only.anchor_at(togi_journal::Seq::new(2).unwrap()).unwrap();
    let got = after(&[only], &anchor).unwrap();
    assert_eq!(got.status, Status::Concluded);
    assert_eq!((got.crashes, got.hours), (1, 3.0));
}
