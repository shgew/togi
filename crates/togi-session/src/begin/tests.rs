use std::io;

use togi_carry::{Archives, select};
use togi_config::Config;
use togi_journal::{CorePhase, Event, Notice, Payload, SessionId};
use togi_sim::MachineDefinition;

use super::*;
use crate::testing::{Bench, core, kinds_of, target};

fn begun(bench: &Bench) -> Session {
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
        .filter_map(|event| match &event.payload {
            Payload::CorePhase(phase) => Some((phase.clone(), event.cause.clone())),
            _ => None,
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
        Control::Stop(crate::session::Stop {
            reason: crate::session::StopReason::DeadEnd,
            ..
        })
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
