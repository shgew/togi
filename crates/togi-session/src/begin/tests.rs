use std::io;

use jiff::Timestamp;
use togi_carry::{Archives, PendingCarry, copied_identity, select};
use togi_config::Config;
use togi_journal::fs::{Fault, Op};
use togi_journal::{
    Activity, Attribution, CorePhase, Event, Failure, Notice, Outcome, Payload, SessionId,
    SessionStart, Stamp, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{BootId, Condition, Signal};
use togi_sim::MachineDefinition;

use super::*;
use crate::testing::{Bench, build, core, kinds_of, target};

fn begun(bench: &Bench) -> Session<'static> {
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    session
}

fn phases(session: &Session) -> Vec<(CoreTransition, Vec<togi_journal::Seq>)> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::CorePhase(phase) = &event.payload {
                Some((phase.clone(), event.cause.clone()))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn a_zero_machine_gets_a_context_a_baseline_from_its_readbacks_and_a_phase_per_core() {
    let bench = Bench::new(2);
    let session = begun(&bench);
    assert_eq!(
        kinds_of(session.appender())[2..],
        [
            "session.context",
            "smu.readback",
            "smu.readback",
            "session.baseline",
            "core.phase",
            "core.phase"
        ]
    );
    let events = session.appender().journal().events();
    let baseline = &events[5];
    assert_eq!(baseline.cause, [events[3].seq, events[4].seq]);
    let phases = phases(&session);
    for (n, (phase, causes)) in phases.iter().enumerate() {
        assert_eq!(phase.core, core(i64::try_from(n).unwrap()));
        assert_eq!(phase.to, CorePhase::Search);
        assert_eq!(phase.offset, target(0));
        assert_eq!(phase.reason, "baseline");
        assert!(!phase.check_solo_limit && phase.workloads.is_empty());
        assert_eq!(causes, &[baseline.seq]);
    }
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn beginning_again_records_nothing() {
    let bench = Bench::new(2);
    let mut session = begun(&bench);
    let before = kinds_of(session.appender());
    assert_eq!(session.begin().unwrap(), Control::Continue);
    assert_eq!(kinds_of(session.appender()), before);
    drop(session);
    let mut again = bench.open();
    again.start().unwrap();
    let before = kinds_of(again.appender());
    again.begin().unwrap();
    assert_eq!(kinds_of(again.appender()), before);
}

#[test]
fn a_nonzero_baseline_is_noticed_once_citing_the_baseline_and_is_where_the_search_starts() {
    let bench = Bench::defined(&MachineDefinition {
        cores: 2,
        bios: Some(vec![-5, 0]),
        ..MachineDefinition::default()
    });
    let session = begun(&bench);
    let events = session.appender().journal().events();
    let baseline = events
        .iter()
        .find(|event| matches!(event.payload, Payload::SessionBaseline(_)))
        .unwrap();
    let notice = events
        .iter()
        .find(|event| matches!(event.payload, Payload::SessionNotice(_)))
        .unwrap();
    assert_eq!(notice.cause, [baseline.seq]);
    let Payload::SessionNotice(payload) = &notice.payload else {
        unreachable!();
    };
    assert_eq!(payload.notice, Notice::NonzeroBaseline);
    assert_eq!(payload.cores.iter().collect::<Vec<_>>(), [core(0)]);
    assert_eq!(phases(&session)[0].0.offset, target(-5));
}

#[test]
fn a_configured_start_and_a_configured_candidate_override_the_baseline() {
    let mut bench = Bench::new(2);
    bench.config =
        Config::parse(r#"{"start_offsets":{"0":-20},"candidate_solo_limits":{"1":-30}}"#).unwrap();
    let session = begun(&bench);
    let phases = phases(&session);
    let (first, _) = &phases[0];
    assert_eq!(first.offset, target(-20));
    assert_eq!(first.reason, "configured start offset");
    assert!(!first.check_solo_limit);
    let (second, _) = &phases[1];
    assert_eq!(second.offset, target(-30));
    assert_eq!(second.reason, "configured candidate solo limit");
    assert!(second.check_solo_limit);
    assert_eq!(
        second
            .workloads
            .iter()
            .map(|workload| workload.as_str())
            .collect::<Vec<_>>(),
        [workloads(Regime::R1)[0].id, workloads(Regime::R2)[0].id]
    );
}

#[test]
fn a_baseline_failing_to_read_is_smu_evidence_and_leaves_no_baseline() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    bench
        .sim
        .interrupt_smu(|operation: &togi_sim::faults::SmuOperation| {
            if operation.op == togi_sim::faults::SmuOp::Read {
                return Err(togi_machine::Error::Other("mailbox timeout".into()));
            }
            Ok(())
        });
    let control = session.begin().unwrap();
    assert!(matches!(
        control,
        Control::Stop(crate::session::Stop::DeadEnd { .. })
    ));
    assert!(session.appender().recovery().baseline().is_none());
    assert!(phases(&session).is_empty());
}

struct Held(Vec<Event>);

impl Archives for Held {
    type Error = io::Error;
    type Events = Vec<Result<Event, io::Error>>;

    fn events(&mut self, _: SessionId) -> Result<Option<Self::Events>, io::Error> {
        Ok(Some(self.0.iter().cloned().map(Ok).collect()))
    }
}

#[test]
fn a_carry_is_committed_after_the_context_and_baseline_and_before_any_phase_once() {
    let first = Bench::new(2);
    let archived = begun(&first);
    let events = archived.appender().journal().events().to_vec();
    let Payload::SessionStart(start) = &events[0].payload else {
        unreachable!();
    };
    let id = start.session;
    let pending = select(&[id], None, &[], &mut Held(events.clone()))
        .unwrap()
        .unwrap();
    let bench = Bench::new(2);
    let mut input = bench.input();
    input.carry = Some(pending);
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    session.start().unwrap();
    session.begin().unwrap();
    let kinds = kinds_of(session.appender());
    let carried = kinds
        .iter()
        .position(|kind| *kind == "session.carried")
        .unwrap();
    let baseline = kinds
        .iter()
        .position(|kind| *kind == "session.baseline")
        .unwrap();
    let phase = kinds.iter().position(|kind| *kind == "core.phase").unwrap();
    assert!(baseline < carried && carried < phase);
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "session.carried")
            .count(),
        1
    );
    let before = kinds.len();
    session.begin().unwrap();
    assert_eq!(kinds_of(session.appender()).len(), before);
    crate::testing::assert_fold_consistent(session.appender());
}

/// An archived session of the bench's machine and BIOS holding three decisive facts: an alone pass, an alone failure
/// of core 1 and a failure at idle. Returns its events and the identity each fact carries into the next session.
fn archive_with_facts(bench: &Bench) -> (SessionId, Vec<Event>, Vec<u64>) {
    let machine = bench.sim.seams();
    let mut host = machine.host;
    let id: SessionId = "20260101T000000Z".parse().unwrap();
    let mut events = Vec::new();
    let mut add = |payload: Payload, cause: &[togi_journal::Seq]| {
        let seq = togi_journal::Seq::new(events.len() as u64 + 1).unwrap();
        let stamp = Stamp {
            time: Timestamp::from_second(1_700_000_000 + i64::try_from(seq.get()).unwrap())
                .unwrap(),
            mono: None,
        };
        events.push(Event::new(
            seq,
            stamp,
            BootId::from("archived".to_owned()),
            cause.to_vec(),
            payload,
        ));
        seq
    };
    add(
        Payload::SessionStart(SessionStart {
            build: build(),
            session: id,
            cores: host.topology().unwrap(),
        }),
        &[],
    );
    add(
        Payload::SessionContext(SessionContext::from(host.bios_context().unwrap())),
        &[],
    );
    let workload = workloads(Regime::R1)[0].id();
    let intent = |trial: &str, on: i64, at: i64| TrialIntent {
        trial: TrialId::from(trial.to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        profile: (0..2)
            .map(|n| (core(n), target(if n == on { at } else { 0 })))
            .collect(),
        core: Some(core(on)),
        cores: CoreSet::default(),
        offset: Some(target(at)),
        regime: Regime::R1,
        workload,
        duration_s: 90,
        condition: Condition::Alone,
        activity: Some(Activity::Search),
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    };
    let end = |trial: &str, outcome: Outcome, signal: Option<Signal>, on: i64| TrialEnd {
        signal,
        core: signal.map(|_| core(on)),
        ..crate::evidence::trial_end(
            TrialId::from(trial.to_owned()),
            outcome,
            if outcome == Outcome::Failure { 11 } else { 90 },
        )
    };
    add(Payload::TrialIntent(intent("0001", 0, -12)), &[]);
    let pass = add(Payload::TrialEnd(end("0001", Outcome::Pass, None, 0)), &[]);
    add(Payload::TrialIntent(intent("0002", 1, -30)), &[]);
    let failed = add(
        Payload::TrialEnd(end(
            "0002",
            Outcome::Failure,
            Some(Signal::UnexpectedExit),
            1,
        )),
        &[],
    );
    add(
        Payload::Failure(Failure {
            signal: Signal::UnexpectedExit,
            attribution: Attribution::Attributed,
            core: Some(core(1)),
            offset: Some(target(-30)),
            trial: Some(TrialId::from("0002".to_owned())),
            regime: Some(Regime::R1),
            condition: Some(Condition::Alone),
            profile: None,
            known_failure: None,
            reason: None,
            round: None,
        }),
        &[failed],
    );
    let idle = add(
        Payload::Failure(Failure {
            signal: Signal::Crash,
            attribution: Attribution::Unattributed,
            core: None,
            offset: None,
            trial: None,
            regime: None,
            condition: Some(Condition::Together),
            profile: Some((0..2).map(|n| (core(n), target(-12))).collect()),
            known_failure: None,
            reason: None,
            round: None,
        }),
        &[],
    );
    (id, events, vec![pass.get(), failed.get(), idle.get()])
}

fn pending_carry(bench: &Bench) -> (PendingCarry, SessionId, Vec<u64>) {
    let (id, events, sources) = archive_with_facts(bench);
    let pending = select(&[id], None, &[], &mut Held(events))
        .unwrap()
        .unwrap();
    (pending, id, sources)
}

/// One run's start and begin with the carry, on a fresh session over the bench's journal.
fn carried_run(
    bench: &Bench,
    pending: &PendingCarry,
) -> (Session<'static>, Result<(), SessionError>) {
    let mut input = bench.input();
    input.carry = Some(pending.clone());
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    let result = session.start().and_then(|_| session.begin()).map(drop);
    (session, result)
}

/// What the journal carries: the identity of each copied fact in order, the commitments, and whether the order
/// context, facts, commitment, first phase held.
fn carried_shape(events: &[Event]) -> (Vec<(SessionId, togi_journal::Seq)>, usize) {
    let mut facts = Vec::new();
    let mut commitments = 0;
    let mut context = None;
    let mut commit = None;
    let mut phase = None;
    for event in events {
        let payload = &event.payload;
        if let Payload::SessionContext(_) = payload {
            assert!(context.is_none(), "a second context");
            context = Some(event.seq);
        } else if let Payload::SessionCarried(_) = payload {
            commitments += 1;
            commit.get_or_insert(event.seq);
        } else if let Payload::CorePhase(_) = payload {
            phase.get_or_insert(event.seq);
        } else if let Some(fact) = copied_identity(payload) {
            assert!(
                commit.is_none() && phase.is_none(),
                "fact {} after the commitment",
                event.seq
            );
            assert!(context.is_some(), "fact {} before the context", event.seq);
            facts.push((fact.session, fact.seq));
        }
    }
    let (Some(commit), Some(phase)) = (commit, phase) else {
        panic!("no commitment or no phase: {:?}", kinds_of_events(events));
    };
    assert!(commit < phase, "the commitment follows the first phase");
    (facts, commitments)
}

fn kinds_of_events(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|event| event.kind().as_str()).collect()
}

#[test]
fn every_eligible_fact_is_carried_once_in_order_before_one_commitment_and_the_first_phase() {
    let bench = Bench::new(2);
    let (pending, id, sources) = pending_carry(&bench);
    let (session, result) = carried_run(&bench, &pending);
    result.unwrap();
    let (facts, commitments) = carried_shape(session.appender().journal().events());
    let want: Vec<_> = sources
        .iter()
        .map(|seq| (id, togi_journal::Seq::new(*seq).unwrap()))
        .collect();
    assert_eq!(facts, want);
    assert_eq!(commitments, 1);
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn a_begin_interrupted_before_or_after_any_append_up_to_the_commitment_resumes_to_one_complete_carry()
 {
    let reference = Bench::new(2);
    let (pending, _, sources) = pending_carry(&reference);
    let (done, result) = carried_run(&reference, &pending);
    result.unwrap();
    let events = done.appender().journal().events();
    let commit = events
        .iter()
        .position(|event| matches!(event.payload, Payload::SessionCarried(_)))
        .unwrap();
    let committed = events[commit].payload.clone();
    let (reference_facts, _) = carried_shape(events);
    assert_eq!(reference_facts.len(), sources.len());
    assert_eq!(
        events
            .iter()
            .filter(|e| copied_identity(&e.payload).is_some())
            .count(),
        3
    );
    let appends = u64::try_from(commit).unwrap() + 1;
    assert!(
        appends > 5,
        "the fixture should put several appends before the commitment"
    );
    drop(done);

    for after in [false, true] {
        for nth in 1..=appends {
            let bench = Bench::new(2);
            let fault = if after {
                Fault::after(Op::Write, nth)
            } else {
                Fault::before(Op::Write, nth)
            };
            bench.mem.inject(fault.named("events.jsonl"));
            let (interrupted, result) = carried_run(&bench, &pending);
            assert!(
                matches!(
                    result,
                    Err(SessionError::Journal(_)
                        | SessionError::Write(crate::write::WriteFailure::Journal(_)))
                ),
                "write {nth} (after {after}): {result:?}"
            );
            if !after {
                let held = interrupted.appender().journal().events().len() as u64;
                assert_eq!(held, nth - 1, "write {nth}");
            }
            drop(interrupted);
            bench.mem.clear_faults();

            let (resumed, result) = carried_run(&bench, &pending);
            result.unwrap();
            let events = resumed.appender().journal().events();
            let (facts, commitments) = carried_shape(events);
            assert_eq!(facts, reference_facts, "write {nth} (after {after})");
            assert_eq!(commitments, 1, "write {nth} (after {after})");
            let resumed_commit = events
                .iter()
                .find(|event| matches!(event.payload, Payload::SessionCarried(_)))
                .unwrap();
            assert_eq!(
                resumed_commit.payload, committed,
                "write {nth} (after {after})"
            );
            assert_eq!(phases(&resumed).len(), 2, "write {nth} (after {after})");
            crate::testing::assert_fold_consistent(resumed.appender());
        }
    }
}

fn carried_core(limit: Option<i64>, failure_point: Option<i64>) -> togi_journal::CarriedCore {
    let from = togi_journal::Provenance {
        session: SessionId::new(jiff::Timestamp::from_second(1_000_000).unwrap(), None).unwrap(),
        seq: togi_journal::Seq::new(1).unwrap(),
    };
    togi_journal::CarriedCore {
        core: core(0),
        candidate_solo_limit: limit.map(|offset| togi_journal::CarriedLimit {
            offset: target(offset),
            from,
        }),
        failure_point: failure_point.map(|offset| togi_journal::CarriedFailurePoint {
            offset: target(offset),
            from,
            signal: togi_machine::Signal::Crash,
        }),
    }
}

#[test]
fn a_carried_start_stays_one_count_shallower_than_the_failure_point() {
    let bench = Bench::new(2);
    let session = bench.open();
    let carried = carried_core(Some(-12), Some(-10));
    let phase = session.first_transition(core(0), 0, Some(&carried));
    assert_eq!(phase.offset, target(-9));
    assert!(phase.check_solo_limit);
    assert_eq!(phase.failure_point, Some(target(-10)));
    assert!(
        phase
            .reason
            .contains("one count shallower than the failure point -10 carried from session")
    );

    let carried = carried_core(Some(-5), Some(-10));
    let phase = session.first_transition(core(0), 0, Some(&carried));
    assert_eq!(phase.offset, target(-5));
    assert_eq!(phase.failure_point, Some(target(-10)));

    let carried = carried_core(None, Some(-20));
    let phase = session.first_transition(core(0), 0, Some(&carried));
    assert_eq!(phase.offset, target(0));
    assert!(!phase.check_solo_limit);
    assert_eq!(phase.failure_point, Some(target(-20)));
}

#[test]
fn a_resumed_baseline_beyond_the_range_is_clamped_where_the_search_starts() {
    let bench = Bench::new(2);
    let mut session = crate::testing::swept(&bench);
    crate::testing::recorded_baseline(&mut session, &[-60, 5]);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    let phases = phases(&session);
    let offsets: Vec<_> = phases.iter().map(|(phase, _)| phase.offset).collect();
    assert_eq!(offsets, [target(-50), target(0)]);
    let reasons: Vec<_> = phases
        .iter()
        .map(|(phase, _)| phase.reason.as_str())
        .collect();
    assert_eq!(
        reasons,
        ["baseline -60 clamped to -50", "baseline 5 clamped to 0"]
    );
}
