use togi_journal::{Build, Event, Seq};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use super::support::{
    A, B, build, config_loaded, config_loaded_by, crash, events, extend, run, session,
    shutdown_cycles, start, start_with,
};
use crate::{Error, Flag, Placement, Record, Score, Session, Status};

fn before(rev: &str) -> Vec<Event> {
    events(1, start(A, rev))
}

fn anchored(base: &[Event], commit: &str, runs: Vec<crate::Run>) -> Record {
    let anchor = Session::from_events(base.to_vec())
        .unwrap()
        .anchor_at(Seq::new(2).unwrap())
        .unwrap();
    Record::new(anchor, commit, false, Vec::new(), runs).unwrap()
}

type Case = (&'static str, Vec<Session>, Vec<Build>, Vec<Flag>);

type Scored = (String, f64, bool, Placement, Option<f64>, Option<f64>);

fn rows(score: &Score<'_>) -> Vec<Scored> {
    score
        .rows
        .iter()
        .map(|row| {
            (
                row.metric.kind.to_string(),
                row.actual,
                row.bound,
                row.placement,
                row.error,
                row.percentile,
            )
        })
        .collect()
}

fn row(name: &str, actual: f64, bound: bool, placement: Placement, error: f64, pct: f64) -> Scored {
    (
        name.to_owned(),
        actual,
        bound,
        placement,
        Some(error),
        Some(pct),
    )
}

fn ensemble() -> Vec<crate::Run> {
    vec![
        run(Status::Concluded, 2.0, 0, 0, &[-12, -20]),
        run(Status::Concluded, 4.0, 1, 1, &[-10, -22]),
        run(Status::DeadEnd, 6.0, 2, 1, &[-8, -24]),
    ]
}

#[test]
fn a_concluded_run_with_the_forecasts_build_is_placed_against_the_ensemble() {
    let base = before("abc1234");
    let record = anchored(&base, "abc1234", ensemble());
    let after = [config_loaded("abc1234"), crash(), shutdown_cycles()];
    let score = record.score(&[extend(&base, 3, after)]).unwrap();
    assert_eq!(
        rows(&score),
        [
            row("core 00", -10.0, false, Placement::Inside, 0.0, 50.0),
            row(
                "core 01",
                -20.0,
                false,
                Placement::Inside,
                2.0,
                100.0 * 2.5 / 3.0
            ),
            row("depth", -30.0, false, Placement::Above, 2.0, 100.0),
            row("hours", 3.0, false, Placement::Inside, 0.0, 50.0),
            row("crashes", 1.0, false, Placement::Inside, 0.0, 50.0),
            row(
                "hunts",
                0.0,
                false,
                Placement::Inside,
                -1.0,
                100.0 * 0.5 / 3.0
            ),
        ]
    );
    assert_eq!(score.flags, []);
    assert_eq!(score.outcome.status, Status::Concluded);
}

#[test]
fn a_censored_run_bounds_hours_crashes_and_hunts() {
    let base = before("abc1234");
    let record = anchored(&base, "abc1234", ensemble());
    let after = [config_loaded("abc1234"), crash(), crash(), crash()];
    let score = record.score(&[extend(&base, 3, after)]).unwrap();
    assert_eq!(
        rows(&score),
        [
            row("core 00", -10.0, false, Placement::Inside, 0.0, 50.0),
            row(
                "core 01",
                -20.0,
                false,
                Placement::Inside,
                2.0,
                100.0 * 2.5 / 3.0
            ),
            row("depth", -30.0, false, Placement::Above, 2.0, 100.0),
            row("hours", 4.0, true, Placement::Open, 1.0, 50.0),
            row("crashes", 3.0, true, Placement::Above, 2.0, 100.0),
            row("hunts", 0.0, true, Placement::Open, -1.0, 0.0),
        ]
    );
}

#[test]
fn another_build_is_flagged() {
    let base = before("abc1234");
    let record = anchored(&base, "abc1234", ensemble());
    let after = [config_loaded("fedcba9-dirty"), shutdown_cycles()];
    let score = record.score(&[extend(&base, 3, after)]).unwrap();
    assert_eq!(
        rows(&score),
        [
            row("core 00", -10.0, false, Placement::Inside, 0.0, 50.0),
            row(
                "core 01",
                -20.0,
                false,
                Placement::Inside,
                2.0,
                100.0 * 2.5 / 3.0
            ),
            row("depth", -30.0, false, Placement::Above, 2.0, 100.0),
            row("hours", 2.0, false, Placement::Inside, -1.0, 25.0),
            row(
                "crashes",
                0.0,
                false,
                Placement::Inside,
                -1.0,
                100.0 * 0.5 / 3.0
            ),
            row(
                "hunts",
                0.0,
                false,
                Placement::Inside,
                -1.0,
                100.0 * 0.5 / 3.0
            ),
        ]
    );
    assert_eq!(
        score
            .flags
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["build revision fedcba9-dirty ran after the anchor; the forecast is at commit abc1234"]
    );
}

#[test]
fn without_concluded_runs_hours_are_unforecast() {
    let base = before("abc1234");
    let runs = vec![
        run(Status::DeadEnd, 6.0, 2, 1, &[-8, -24]),
        run(Status::Censored, 5.0, 1, 0, &[-10, -20]),
    ];
    let record = anchored(&base, "abc1234", runs);
    let after = [config_loaded("abc1234"), crash()];
    let score = record.score(&[extend(&base, 3, after)]).unwrap();
    assert_eq!(
        rows(&score),
        [
            row("core 00", -10.0, false, Placement::Inside, -1.0, 25.0),
            row("core 01", -20.0, false, Placement::Inside, 2.0, 75.0),
            row("depth", -30.0, false, Placement::Inside, 1.0, 75.0),
            (
                "hours".to_owned(),
                2.0,
                true,
                Placement::Unforecast,
                None,
                None
            ),
            row("crashes", 1.0, true, Placement::Open, -0.5, 0.0),
            row("hunts", 0.0, true, Placement::Open, -0.5, 0.0),
        ]
    );
}

#[test]
fn a_run_over_other_cores_than_the_forecast_is_refused() {
    let base = before("abc1234");
    let record = anchored(
        &base,
        "abc1234",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20, -30])],
    );
    let error = record
        .score(&[extend(&base, 3, [shutdown_cycles()])])
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "run has cores [0, 1], forecast [0, 1, 2]"
    );
    assert!(matches!(error, Error::ScoreCores { .. }));
}

#[test]
fn a_dirty_forecast_and_another_ruleset_or_evidence_epoch_are_flagged() {
    let base = before("abc1234");
    let mut record = anchored(
        &base,
        "abc1234",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20])],
    );
    record.dirty = true;
    record.ruleset = RULESET - 1;
    record.evidence = EVIDENCE_EPOCH + 1;
    let score = record
        .score(&[extend(&base, 3, [config_loaded("abc1234")])])
        .unwrap();
    assert_eq!(
        score.flags,
        [
            Flag::DirtyForecast {
                commit: "abc1234".to_owned()
            },
            Flag::Ruleset {
                ran: RULESET,
                forecast: RULESET - 1
            },
            Flag::EvidenceEpoch {
                ran: EVIDENCE_EPOCH,
                forecast: EVIDENCE_EPOCH + 1
            },
        ]
    );
    assert_eq!(
        score.flags[1].to_string(),
        format!(
            "ruleset {RULESET} ran after the anchor; the forecast used ruleset {}",
            RULESET - 1
        )
    );
    assert_eq!(
        score.flags[0].to_string(),
        "the forecast was made from a dirty tree at commit abc1234"
    );
    assert_eq!(
        score.flags[2].to_string(),
        format!(
            "evidence epoch {EVIDENCE_EPOCH} ran after the anchor; the forecast used epoch {}",
            EVIDENCE_EPOCH + 1
        )
    );
}

#[test]
fn a_build_without_a_revision_or_a_dirty_one_is_never_the_forecasts_commit() {
    let base = before("abc1234");
    let record = anchored(
        &base,
        "abc1234",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20])],
    );
    let mut bare: Build = build("abc1234");
    bare.rev = None;
    let score = record
        .score(&[extend(&base, 3, [config_loaded_by(bare)])])
        .unwrap();
    assert_eq!(
        score.flags,
        [Flag::Revision {
            rev: String::new(),
            commit: "abc1234".to_owned()
        }]
    );
    let abbreviated = anchored(
        &base,
        "abc12345678",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20])],
    );
    let score = abbreviated
        .score(&[extend(&base, 3, [config_loaded("abc1234")])])
        .unwrap();
    assert_eq!(score.flags, []);
}

#[test]
fn builds_cover_the_scored_span() {
    let base = before("0123456");
    let record = anchored(
        &base,
        "abc1234",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20])],
    );
    let in_effect = build("0123456");
    let forecast = build("abc1234");
    let flag = Flag::Revision {
        rev: "0123456".to_owned(),
        commit: "abc1234".to_owned(),
    };
    let cases: Vec<Case> = vec![
        (
            "the invocation running at the anchor keeps writing",
            vec![extend(&base, 3, [crash(), shutdown_cycles()])],
            vec![in_effect.clone()],
            vec![flag.clone()],
        ),
        (
            "the invocation running at the anchor writes before a new stamp",
            vec![extend(
                &base,
                3,
                [crash(), config_loaded("abc1234"), shutdown_cycles()],
            )],
            vec![in_effect, forecast.clone()],
            vec![flag],
        ),
        (
            "a new stamp right after the anchor",
            vec![extend(
                &base,
                3,
                [config_loaded("abc1234"), crash(), shutdown_cycles()],
            )],
            vec![forecast.clone()],
            vec![],
        ),
        (
            "a new session after the anchor",
            vec![
                Session::from_events(base.clone()).unwrap(),
                session(1, [start_with(B, build("abc1234")), shutdown_cycles()]),
            ],
            vec![forecast.clone()],
            vec![],
        ),
        (
            "stamps after the terminal point are ignored",
            vec![extend(
                &base,
                3,
                [
                    config_loaded("abc1234"),
                    shutdown_cycles(),
                    config_loaded("fedcba9"),
                ],
            )],
            vec![forecast],
            vec![],
        ),
    ];
    for (name, sessions, builds, flags) in cases {
        let score = record.score(&sessions).unwrap();
        assert_eq!(score.builds, builds, "{name}");
        assert_eq!(score.flags, flags, "{name}");
    }
}

#[test]
fn a_session_the_anchor_does_not_name_or_that_changed_is_refused() {
    let base = before("abc1234");
    let record = anchored(
        &base,
        "abc1234",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20])],
    );
    let mut changed = start(A, "abc1234");
    changed[1] = super::support::baseline([-11, -20]);
    let truncated = session(1, start(A, "abc1234").into_iter().take(1));
    let other = session(1, start(B, "abc1234"));
    let changed = session(1, changed);
    assert!(matches!(record.score(&[]), Err(Error::NoSession { .. })));
    assert!(matches!(
        record.score(&[other]),
        Err(Error::NoSession { .. })
    ));
    assert!(matches!(
        record.score(&[truncated]),
        Err(Error::NoEvent { .. })
    ));
    assert!(matches!(
        record.score(&[changed]),
        Err(Error::ChangedAnchor { .. })
    ));
}

#[test]
fn the_error_messages_name_the_session_and_event() {
    let base = before("abc1234");
    let record = anchored(
        &base,
        "abc1234",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20])],
    );
    let other = session(1, start(B, "abc1234"));
    assert_eq!(
        record.score(&[other]).unwrap_err().to_string(),
        format!("anchor not found: no journal holds session {A}")
    );
    let truncated = session(1, start(A, "abc1234").into_iter().take(1));
    assert_eq!(
        record.score(&[truncated]).unwrap_err().to_string(),
        format!("anchor not found: session {A} has no event 2")
    );
    let mut changed = start(A, "abc1234");
    changed[1] = super::support::baseline([-11, -20]);
    assert_eq!(
        record
            .score(&[session(1, changed)])
            .unwrap_err()
            .to_string(),
        format!("anchor changed: session {A} through event 2 differs from the forecast's copy")
    );
}

#[test]
fn a_score_refuses_sessions_with_another_ruleset_at_its_terminal_session() {
    let base = before("abc1234");
    let record = anchored(
        &base,
        "abc1234",
        vec![run(Status::Concluded, 0.0, 0, 0, &[-10, -20])],
    );
    let mut stale = build("abc1234");
    stale.ruleset = 0;
    let anchor_session = session(
        1,
        [start_with(A, stale), super::support::baseline([-10, -20])],
    );
    let mut record = record;
    record.anchor = anchor_session.anchor_at(Seq::new(2).unwrap()).unwrap();
    assert_eq!(
        record.score(&[anchor_session]).unwrap_err().to_string(),
        format!("session {A}, ruleset 0, cannot be replayed by this build's ruleset {RULESET}")
    );
}
