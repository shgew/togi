use togi_defect::{Clause, Entry};
use togi_journal::{
    CcdEdge, DefectAnswer, DefectAnswered, DefectDirection, DefectId, Edge, EventKind,
    SessionWarning,
};

use super::*;
use crate::session::Control;
use crate::testing::{Bench, core, kinds_of};
use test_case::test_case;

const DEEPER_ON_WARNING: Clause = Clause::DirectCause {
    decision_kind: EventKind::CcdEdge,
    cause_kind: EventKind::SessionWarning,
};

const fn entry(id: u32, direction: DefectDirection) -> Entry {
    Entry {
        id: DefectId::new(id).unwrap(),
        title: "synthetic",
        detail: "synthetic detail",
        pr: 100,
        direction,
        clauses: &[DEEPER_ON_WARNING],
    }
}

static CAUTIOUS: [Entry; 1] = [entry(1, DefectDirection::TooCautious)];
static AGGRESSIVE: [Entry; 1] = [entry(1, DefectDirection::TooAggressive)];

fn session_with_a_match(defects: &'static [Entry]) -> Session<'static> {
    let mut bench = Bench::new(2);
    bench.defects = defects;
    let Ok(mut session) = Session::open(bench.input_unattested()) else {
        panic!("open");
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    let warning = session
        .appender
        .append(
            Payload::SessionWarning(SessionWarning {
                operation: "note".into(),
                trial: None,
                error: String::new(),
            }),
            &[],
        )
        .unwrap();
    let decision = CcdEdge {
        ccd: togi_machine::CcdId::new(1).unwrap(),
        edge: Edge::Unknown {},
        old: Edge::Unknown {},
        reason: "synthetic".into(),
    };
    session
        .appender
        .append(Payload::CcdEdge(decision), &[warning])
        .unwrap();
    session
}

fn kinds_after_start(session: &Session) -> Vec<&'static str> {
    kinds_of(session.appender())[4..].to_vec()
}

#[test]
fn a_matching_decision_is_recorded_as_a_finding_citing_it() {
    let mut session = session_with_a_match(&CAUTIOUS);
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(kinds_after_start(&session), ["defect.found"]);
    let events = session.appender().journal().events();
    let found = events.last().unwrap();
    let Payload::DefectFound(payload) = &found.payload else {
        panic!("{:?}", found.payload);
    };
    assert_eq!(payload.cores.iter().collect::<Vec<_>>(), [core(1)]);
    assert_eq!(found.cause, payload.decisions);
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(kinds_after_start(&session), ["defect.found"]);
}

#[test]
fn an_aggressive_finding_is_a_defect_dead_end_naming_the_reset_that_starts_fresh() {
    let mut session = session_with_a_match(&AGGRESSIVE);
    let decision = session.appender().journal().events().last().unwrap().seq;
    let stop = session.check_defects().unwrap().unwrap();
    let Stop::DeadEnd {
        dead_end, causes, ..
    } = stop
    else {
        panic!("{stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::Defect);
    assert_eq!(
        dead_end.detail,
        format!(
            "defect 1 (synthetic) affected decisions [#{decision}] on cores [1]; togi reset --all archives this \
             session and starts fresh"
        )
    );
    let found = session
        .appender()
        .defects()
        .finding_seq(DefectId::new(1).unwrap());
    assert_eq!(Some(causes[0]), found);
}

#[test]
fn a_cautious_finding_lets_the_night_continue() {
    let mut session = session_with_a_match(&CAUTIOUS);
    assert!(session.check_defects().unwrap().is_none());
    assert!(session.check_defects().unwrap().is_none());
}

/// A session reopened on a journal holding a too-aggressive finding of a defect this build does not list, and,
/// when `answer` is given, a `defect.answered` an earlier build recorded for it.
fn resumed_with_an_unlisted_finding(
    answer: Option<DefectAnswer>,
) -> (Bench, Session<'static>, Seq) {
    let bench = Bench::new(2);
    let mut first = bench.open();
    assert_eq!(first.start().unwrap(), Control::Continue);
    let start = first.appender().journal().events()[0].seq;
    let found = DefectFound {
        id: DefectId::new(7).unwrap(),
        title: "unlisted".into(),
        detail: "recorded by a build that listed it".into(),
        pr: 200,
        direction: DefectDirection::TooAggressive,
        cores: [core(1)].into_iter().collect(),
        decisions: vec![start],
    };
    let found = first
        .appender
        .append(Payload::DefectFound(found), &[start])
        .unwrap();
    if let Some(answer) = answer {
        let answered = DefectAnswered {
            id: DefectId::new(7).unwrap(),
            cores: [core(1)].into_iter().collect(),
            answer,
        };
        first
            .appender
            .append(Payload::DefectAnswered(answered), &[found])
            .unwrap();
    }
    drop(first);
    let Ok(mut session) = Session::open(bench.input()) else {
        panic!("reopen");
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    (bench, session, found)
}

#[test_case(None; "unanswered")]
#[test_case(Some(DefectAnswer::No); "an archived no")]
#[test_case(Some(DefectAnswer::Yes); "an archived yes")]
fn a_resumed_aggressive_finding_of_an_unlisted_defect_is_a_defect_dead_end_whatever_was_answered(
    answer: Option<DefectAnswer>,
) {
    let (_bench, mut session, found) = resumed_with_an_unlisted_finding(answer);
    let start = session.appender().journal().events()[0].seq;
    let stop = session.check_defects().unwrap();
    let Some(Stop::DeadEnd {
        dead_end, causes, ..
    }) = stop
    else {
        panic!("{stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::Defect);
    assert_eq!(
        dead_end.detail,
        format!(
            "defect 7 (unlisted) affected decisions [#{start}] on cores [1]; togi reset --all archives this session \
             and starts fresh"
        )
    );
    assert_eq!(causes, [found]);
    assert!(
        !kinds_of(session.appender()).contains(&"command.reset"),
        "no per-core reset is queued"
    );
}
