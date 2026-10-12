use std::cell::Cell;
use std::io;
use std::num::NonZeroU32;
use std::rc::Rc;

use togi_defect::{Clause, DefectId, Entry};
use togi_journal::{
    Activity, Decision, DefectAnswer, DefectDirection, EventKind, SessionWarning, TunerDecision,
};
use togi_journal::fs::{Fault, Op};
use togi_machine::TargetOffset;
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;

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
    let Stop::DeadEnd {
        dead_end, causes, ..
    } = stop
    else {
        panic!("{stop:?}");
    };
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
    assert_eq!(stop, Stop::Signal);
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

/// Answers every question with `answer`, counting the questions and checking each names `cores`.
struct Counted {
    answer: bool,
    cores: Vec<togi_machine::CoreId>,
    asked: Rc<Cell<u32>>,
}

impl Prompt for Counted {
    fn reset_cores(&mut self, finding: &togi_defect::FindingView<'_>) -> Result<bool, PromptError> {
        assert_eq!(finding.cores.iter().collect::<Vec<_>>(), self.cores);
        self.asked.set(self.asked.get() + 1);
        Ok(self.answer)
    }
}

const UNLISTED: u32 = 7;

/// A session reopened on a journal holding a too-aggressive finding of a defect this build does not list.
fn resumed_with_an_unlisted_finding(prompt: Option<Box<dyn Prompt>>) -> (Bench, Session, Seq) {
    let bench = Bench::new(2);
    let mut first = bench.open();
    assert_eq!(first.start().unwrap(), Control::Continue);
    let start = first.appender().journal().events()[0].seq;
    let found = DefectFound {
        id: DefectId::new(UNLISTED).unwrap().0,
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
    drop(first);
    let mut input = bench.input();
    input.prompt = prompt;
    let Ok(mut session) = Session::open(input) else {
        panic!("reopen");
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    (bench, session, found)
}

#[test]
fn a_resumed_aggressive_finding_of_an_unlisted_defect_without_a_terminal_is_a_defect_dead_end() {
    let (_bench, mut session, found) = resumed_with_an_unlisted_finding(None);
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
        "defect 7 (unlisted) affected cores [1]; run in a terminal to decide whether to reset them"
    );
    assert_eq!(causes, [found]);
}

#[test]
fn a_resumed_finding_of_an_unlisted_defect_is_asked_and_its_yes_resets_the_recorded_cores() {
    let asked = Rc::new(Cell::new(0));
    let prompt = Counted {
        answer: true,
        cores: vec![core(1)],
        asked: Rc::clone(&asked),
    };
    let (_bench, mut session, found) = resumed_with_an_unlisted_finding(Some(Box::new(prompt)));
    let before = session.appender().journal().events().len();
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(asked.get(), 1);
    let events = session.appender().journal().events();
    let appended: Vec<_> = events[before..]
        .iter()
        .map(|event| (event.payload.clone(), event.cause.clone()))
        .collect();
    let answer = events[before].seq;
    assert_eq!(
        appended,
        [
            (
                Payload::DefectAnswered(DefectAnswered {
                    id: DefectId::new(UNLISTED).unwrap().0,
                    cores: [core(1)].into_iter().collect(),
                    answer: DefectAnswer::Yes,
                }),
                vec![found],
            ),
            (
                Payload::CommandReset(CommandReset {
                    target: ResetTarget::Core(core(1)),
                }),
                vec![answer],
            ),
        ]
    );
}

fn steady() -> Bench {
    Bench::defined(&MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    })
}

fn run_to(bench: &Bench, cycles: u32) {
    let mut input = bench.input();
    input.policy.cycles = NonZeroU32::new(cycles);
    let ended = crate::run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
}

/// A session that has run a checking cycle and then holds a finding of a defect this build does not list, over
/// both cores.
fn tuned_with_an_unlisted_finding_over_two_cores(bench: &Bench) -> Seq {
    run_to(bench, 1);
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    let cited = session.appender().journal().events()[0].seq;
    let found = DefectFound {
        id: DefectId::new(UNLISTED).unwrap().0,
        title: "unlisted".into(),
        detail: "recorded by a build that listed it".into(),
        pr: 200,
        direction: DefectDirection::TooAggressive,
        cores: [core(0), core(1)].into_iter().collect(),
        decisions: vec![cited],
    };
    session
        .appender
        .append(Payload::DefectFound(found), &[cited])
        .unwrap()
}

fn yes_for_both_cores(asked: &Rc<Cell<u32>>) -> Option<Box<dyn Prompt>> {
    Some(Box::new(Counted {
        answer: true,
        cores: vec![core(0), core(1)],
        asked: Rc::clone(asked),
    }))
}

type Recorded = (Seq, Payload, Vec<Seq>);

fn after(bench: &Bench, found: Seq) -> Vec<Recorded> {
    let boot = bench.sim.seams().host.boot_id().unwrap();
    let journal = crate::testing::simulated(&bench.mem)
        .open_at(boot, bench.sim.now())
        .unwrap();
    journal
        .events()
        .iter()
        .filter(|event| event.seq > found)
        .map(|event| (event.seq, event.payload.clone(), event.cause.clone()))
        .collect()
}

fn opened_with(bench: &Bench, asked: &Rc<Cell<u32>>) -> Session {
    let mut input = bench.input();
    input.prompt = yes_for_both_cores(asked);
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    session
}

/// The answer and the two resets that `check_defects` appends, as an uninterrupted answer leaves them, and the
/// searches the two cores restart because of them.
fn uninterrupted_answer() -> Vec<Payload> {
    let bench = steady();
    let found = tuned_with_an_unlisted_finding_over_two_cores(&bench);
    let asked = Rc::new(Cell::new(0));
    let mut session = opened_with(&bench, &asked);
    assert!(session.check_defects().unwrap().is_none());
    assert_eq!(asked.get(), 1);
    drop(session);
    run_to(&bench, 2);
    effects_of_the_answer(&after(&bench, found), found)
}

/// What the answer caused, once and citing what it must: the answer, a reset of each core citing it, and each
/// core's search restarted citing its reset and reaching its limit again. Returns those events' payloads.
fn effects_of_the_answer(recorded: &[Recorded], found: Seq) -> Vec<Payload> {
    let answers: Vec<_> = recorded
        .iter()
        .filter(|(_, payload, _)| matches!(payload, Payload::DefectAnswered(_)))
        .collect();
    assert_eq!(answers.len(), 1, "one answer");
    let (answer, _, cause) = answers[0];
    assert_eq!(cause, &[found]);
    let answer = *answer;
    let resets: Vec<_> = recorded
        .iter()
        .filter(|(_, payload, _)| matches!(payload, Payload::CommandReset(_)))
        .map(|(seq, payload, cause)| {
            let Payload::CommandReset(reset) = payload else {
                unreachable!()
            };
            assert_eq!(cause, &[answer]);
            (*seq, reset.target)
        })
        .collect();
    assert_eq!(
        resets.iter().map(|(_, target)| *target).collect::<Vec<_>>(),
        [ResetTarget::Core(core(0)), ResetTarget::Core(core(1))]
    );
    for (reset, target) in resets {
        let ResetTarget::Core(cored) = target else {
            unreachable!()
        };
        let restarted = recorded.iter().filter(|(_, payload, cause)| {
            matches!(payload, Payload::CorePhase(phase)
                if phase.core == cored
                    && phase.from == Some(togi_journal::CorePhase::AtLimit)
                    && phase.to == togi_journal::CorePhase::Search)
                && cause == &[reset]
        });
        assert_eq!(restarted.count(), 1, "core {cored:?} restarts once");
        let last = recorded
            .iter()
            .rev()
            .find_map(|(_, payload, _)| match payload {
                Payload::CorePhase(phase) if phase.core == cored => Some(phase.to),
                _ => None,
            });
        assert_eq!(last, Some(togi_journal::CorePhase::AtLimit), "{cored:?}");
    }
    recorded
        .iter()
        .filter(|(_, payload, _)| {
            matches!(
                payload,
                Payload::DefectAnswered(_) | Payload::CommandReset(_) | Payload::CorePhase(_)
            )
        })
        .map(|(_, payload, _)| payload.clone())
        .collect()
}

/// The answer and the two resets are interrupted at each boundary: before and after the answer and each reset.
/// The journal then resumed by a session and consumed by a run is the uninterrupted one, and the person is asked
/// again only when the answer was not durable.
#[test]
fn an_interrupted_multi_core_answer_resumes_to_the_effects_of_an_uninterrupted_one() {
    let reference = uninterrupted_answer();
    for at in 1..=3u64 {
        for durable in [false, true] {
            let name = format!("append {at}, durable {durable}");
            let bench = steady();
            let found = tuned_with_an_unlisted_finding_over_two_cores(&bench);
            let asked = Rc::new(Cell::new(0));
            let mut session = opened_with(&bench, &asked);
            let op = if durable { Op::Sync } else { Op::Write };
            bench
                .mem
                .inject(Fault::before(op, at).named("events.jsonl"));
            assert!(
                session.check_defects().is_err(),
                "{name}: the interruption did not fire"
            );
            drop(session);
            bench.mem.clear_faults();
            let answer_events = after(&bench, found)
                .iter()
                .filter(|(_, payload, _)| {
                    matches!(
                        payload,
                        Payload::DefectAnswered(_) | Payload::CommandReset(_)
                    )
                })
                .count();
            assert_eq!(
                answer_events as u64,
                at - 1 + u64::from(durable),
                "{name}: answer events durable after the interruption"
            );
            let mut session = opened_with(&bench, &asked);
            assert!(session.check_defects().unwrap().is_none(), "{name}");
            let total = if at == 1 && !durable { 2 } else { 1 };
            assert_eq!(asked.get(), total, "{name}: questions asked");
            drop(session);
            run_to(&bench, 2);
            assert_eq!(asked.get(), total, "{name}: the run asked again");
            let recorded = after(&bench, found);
            assert_eq!(effects_of_the_answer(&recorded, found), reference, "{name}");
        }
    }
}
