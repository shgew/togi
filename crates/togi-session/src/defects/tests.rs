use std::io;

use togi_defect::{Clause, DefectId, Entry};
use togi_journal::{
    Activity, Decision, DefectAnswer, DefectDirection, EventKind, SessionWarning, TunerDecision,
};
use togi_machine::TargetOffset;

use super::*;
use crate::session::{Control, Prompt};
use crate::testing::{Bench, core, kinds_of};

const DEEPER_ON_WARNING: Clause = Clause {
    decision_kind: EventKind::TunerDecision,
    decision: Decision::StepDeeper,
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

struct Asked(Option<Result<bool, PromptError>>);

impl Prompt for Asked {
    fn reset_cores(&mut self, _: &togi_defect::FindingView<'_>) -> Result<bool, PromptError> {
        self.0.take().expect("asked once")
    }
}

fn asked(answer: Result<bool, PromptError>) -> Box<dyn Prompt> {
    Box::new(Asked(Some(answer)))
}

fn session_with_a_match(defects: &'static [Entry], prompt: Option<Box<dyn Prompt>>) -> Session {
    let mut bench = Bench::new(2);
    bench.defects = defects;
    let mut input = bench.input();
    input.prompt = prompt;
    let Ok(mut session) = Session::open(input) else {
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
    let decision = TunerDecision {
        core: core(1),
        activity: Activity::Search,
        decision: Decision::StepDeeper,
        from_offset: TargetOffset::ZERO,
        to_offset: TargetOffset::ZERO,
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: "synthetic".into(),
    };
    session
        .appender
        .append(Payload::TunerDecision(decision), &[warning])
        .unwrap();
    session
}

fn kinds_after_start(session: &Session) -> Vec<&'static str> {
    kinds_of(session.appender())[4..].to_vec()
}

#[test]
fn a_matching_decision_is_recorded_as_a_finding_citing_it() {
    let mut session = session_with_a_match(&CAUTIOUS, None);
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
fn an_aggressive_finding_without_a_terminal_is_a_defect_dead_end() {
    let mut session = session_with_a_match(&AGGRESSIVE, None);
    let stop = session.check_defects().unwrap().unwrap();
    let (dead_end, causes) = stop.dead_end.unwrap();
    assert_eq!(dead_end.condition, DeadEndCondition::Defect);
    assert_eq!(
        dead_end.detail,
        "defect 1 (synthetic) affected cores [1]; run in a terminal to decide whether to reset them"
    );
    let found = session
        .appender()
        .defects()
        .finding_seq(DefectId::new(1).unwrap());
    assert_eq!(Some(causes[0]), found);
}

#[test]
fn a_yes_answer_queues_a_reset_of_every_affected_core_citing_it() {
    let mut session = session_with_a_match(&CAUTIOUS, Some(asked(Ok(true))));
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(
        kinds_after_start(&session),
        ["defect.found", "defect.answered", "command.reset"]
    );
    let events = session.appender().journal().events();
    let answer = &events[events.len() - 2];
    let reset = events.last().unwrap();
    assert_eq!(reset.cause, [answer.seq]);
    let Payload::DefectAnswered(answered) = &answer.payload else {
        panic!("{:?}", answer.payload);
    };
    assert_eq!(answered.answer, DefectAnswer::Yes);
    let Payload::CommandReset(command) = &reset.payload else {
        panic!("{:?}", reset.payload);
    };
    assert_eq!(command.target, ResetTarget::Core(core(1)));
    let length = events.len();
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(session.appender().journal().events().len(), length);
}

#[test]
fn a_no_answer_resets_nothing() {
    let mut session = session_with_a_match(&CAUTIOUS, Some(asked(Ok(false))));
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(
        kinds_after_start(&session),
        ["defect.found", "defect.answered"]
    );
}

#[test]
fn a_cancelled_question_stops_the_run_with_the_finding_unanswered() {
    let mut session = session_with_a_match(&CAUTIOUS, Some(asked(Err(PromptError::Cancelled))));
    let stop = session.check_defects().unwrap().unwrap();
    assert_eq!(stop.reason, StopReason::Signal);
    assert_eq!(kinds_after_start(&session), ["defect.found"]);
    assert!(session.shutdown.is_some());
}

#[test]
fn a_question_that_cannot_be_asked_is_an_error_and_records_no_answer() {
    let mut session = session_with_a_match(
        &CAUTIOUS,
        Some(asked(Err(PromptError::Io(io::Error::other("no tty"))))),
    );
    assert!(matches!(
        session.check_defects(),
        Err(SessionError::Prompt { defect: 1, .. })
    ));
    assert_eq!(kinds_after_start(&session), ["defect.found"]);
}

#[test]
fn a_yes_answer_whose_resets_were_lost_is_finished_without_asking_again() {
    let mut session = session_with_a_match(&CAUTIOUS, None);
    session.check_defects().unwrap();
    let id = DefectId::new(1).unwrap();
    let found = session.appender().defects().finding_seq(id).unwrap();
    let answered = DefectAnswered {
        id: id.0,
        cores: [core(1)].into_iter().collect(),
        answer: DefectAnswer::Yes,
    };
    let answer = session
        .appender
        .append(Payload::DefectAnswered(answered), &[found])
        .unwrap();
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(
        kinds_after_start(&session),
        ["defect.found", "defect.answered", "command.reset"]
    );
    assert_eq!(
        session.appender().journal().events().last().unwrap().cause,
        [answer]
    );
}
