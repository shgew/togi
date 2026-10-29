use std::collections::BTreeMap;

use serde_json::json;
use test_case::test_case;

use togi_journal::{CcdVector, Payload, SmuCommand, SmuOp};
use togi_machine::{
    CcdId, CcdProfile, CcdProfileError, Clock, CoreInfo, Error, Machine, ObservedOffset,
    TargetOffset,
};
use togi_sim::MachineDefinition;
use togi_sim::faults::{HookPhase, SmuOp as SimOp, SmuOperation};

use super::{DayWriteError, StockApplied, apply_day_stock};
use crate::appender::Folds;
use crate::testing::{Rig, core, target};
use crate::write::WriteFailure;

/// Four cores, cores 0 and 1 on CCD 0 and cores 2 and 3 on CCD 1, every core at a nonzero offset.
struct Day {
    rig: Rig,
    clock: Box<dyn Clock>,
    topology: Vec<CoreInfo>,
}

impl Day {
    fn new() -> Self {
        let rig = Rig::defined(&MachineDefinition {
            cores: 4,
            bios: Some(vec![-5, -7, -9, -11]),
            ..MachineDefinition::default()
        });
        let Machine {
            clock, mut host, ..
        } = rig.sim.seams();
        let topology = host.topology().unwrap();
        Self {
            rig,
            clock,
            topology,
        }
    }

    fn apply(&mut self, requested: &CcdVector) -> Result<StockApplied, DayWriteError> {
        apply_day_stock(
            &mut self.rig.appender.journal,
            &*self.clock,
            &mut *self.rig.smu,
            self.rig.lifecycle.stopped().unwrap(),
            &self.topology,
            requested,
            &[],
        )
    }

    fn vector(&self, ccd0: i64, ccd1: i64) -> CcdVector {
        let ccds: BTreeMap<CcdId, TargetOffset> = [
            (CcdId::new(0).unwrap(), target(ccd0)),
            (CcdId::new(1).unwrap(), target(ccd1)),
        ]
        .into();
        CcdVector::expand(CcdProfile::try_from(ccds).unwrap(), &self.topology).unwrap()
    }

    fn commands(&self) -> Vec<SmuCommand> {
        self.rig
            .events()
            .iter()
            .filter_map(|event| {
                if let Payload::SmuIntent(intent) = &event.payload {
                    Some(intent.command())
                } else {
                    None
                }
            })
            .collect()
    }

    fn offsets(&mut self) -> Vec<i64> {
        (0..4).map(|n| self.rig.offset(n)).collect()
    }

    /// The machine and the journal are as they were before any day write.
    fn assert_untouched(&mut self) {
        assert_eq!(self.rig.kinds(), ["session.start"]);
        assert_eq!(self.offsets(), [-5, -7, -9, -11]);
    }
}

#[test]
fn a_nonzero_request_writes_only_zero_through_intent_write_readback() {
    let mut day = Day::new();
    let applied = day.apply(&day.vector(-20, -30)).unwrap();

    assert_eq!(applied.target, day.vector(0, 0));
    assert_eq!(
        applied.readbacks,
        (0..4)
            .map(|n| (core(n), ObservedOffset(0)))
            .collect::<togi_machine::PerCore<_>>()
    );
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
    assert_eq!(
        day.commands(),
        [SmuCommand::SetAll {
            offset: TargetOffset::ZERO
        }]
    );
    assert_eq!(
        day.rig.kinds(),
        [
            "session.start",
            "smu.intent",
            "smu.write",
            "smu.readback",
            "smu.readback",
            "smu.readback",
            "smu.readback",
        ]
    );

    let events = day.rig.events();
    let chain: Vec<_> = events[1..].iter().map(|event| event.seq).collect();
    assert_eq!(applied.transactions, chain);
    assert_eq!(events[2].cause, [events[1].seq]);
    for readback in &events[3..] {
        assert_eq!(readback.cause, [events[2].seq]);
        let Payload::SmuReadback(record) = &readback.payload else {
            panic!("{:?}", readback.payload);
        };
        assert_eq!(
            (record.offset, record.expected),
            (ObservedOffset(0), Some(TargetOffset::ZERO))
        );
    }
    Folds::replay(events).unwrap();
}

#[test]
fn a_stock_request_is_the_same_single_zero_write() {
    let mut day = Day::new();
    let applied = day.apply(&day.vector(0, 0)).unwrap();
    assert_eq!(applied.target, day.vector(0, 0));
    assert_eq!(
        day.commands(),
        [SmuCommand::SetAll {
            offset: TargetOffset::ZERO
        }]
    );
}

/// What a decoded vector can say that no expansion produces.
#[derive(Clone, Copy, Debug)]
enum Tamper {
    /// A core at an offset other than its CCD's.
    Shifted(i64),
    /// A core of the topology with no offset.
    Dropped(i64),
    /// An offset for a core the topology lacks.
    Extra(i64),
}

fn tampered(vector: &CcdVector, tamper: Tamper) -> CcdVector {
    let mut value = serde_json::to_value(vector).unwrap();
    let cores = value["cores"].as_object_mut().unwrap();
    let key_of = |cores: &serde_json::Map<String, serde_json::Value>, n: i64| {
        cores
            .keys()
            .find(|key| key.parse::<i64>() == Ok(n))
            .unwrap()
            .clone()
    };
    match tamper {
        Tamper::Shifted(n) => {
            let key = key_of(cores, n);
            cores.insert(key, json!(-3));
        }
        Tamper::Dropped(n) => {
            let key = key_of(cores, n);
            cores.remove(&key);
        }
        Tamper::Extra(n) => {
            cores.insert(n.to_string(), json!(-10));
        }
    }
    serde_json::from_value(value).unwrap()
}

#[test_case(Tamper::Shifted(1), 1; "a core off its CCD's offset")]
#[test_case(Tamper::Dropped(3), 3; "a core missing")]
#[test_case(Tamper::Extra(9), 9; "a core the machine lacks")]
fn a_malformed_vector_is_refused_before_any_command(tamper: Tamper, bad: i64) {
    let mut day = Day::new();
    let malformed = tampered(&day.vector(-10, -20), tamper);
    let error = day.apply(&malformed).unwrap_err();
    assert!(
        matches!(error, DayWriteError::Expansion { core: found } if found == core(bad)),
        "{error:?}"
    );
    assert!(day.commands().is_empty());
    day.assert_untouched();
}

#[test]
fn a_profile_that_does_not_cover_the_topology_is_refused_before_any_command() {
    let mut day = Day::new();
    let narrow = CcdVector::expand(
        CcdProfile::uniform([CcdId::new(0).unwrap()], target(-10)).unwrap(),
        &day.topology[..2],
    )
    .unwrap();
    let error = day.apply(&narrow).unwrap_err();
    assert!(
        matches!(error, DayWriteError::Profile(CcdProfileError::Missing(ccd)) if ccd == CcdId::new(1).unwrap()),
        "{error:?}"
    );
    day.assert_untouched();

    let wide = day.vector(-10, -20);
    let error = apply_day_stock(
        &mut day.rig.appender.journal,
        &*day.clock,
        &mut *day.rig.smu,
        day.rig.lifecycle.stopped().unwrap(),
        &day.topology[..2],
        &wide,
        &[],
    )
    .unwrap_err();
    assert!(
        matches!(error, DayWriteError::Profile(CcdProfileError::Extra(ccd)) if ccd == CcdId::new(1).unwrap()),
        "{error:?}"
    );
    day.assert_untouched();
}

#[test]
fn a_failed_intent_writes_nothing_and_the_journal_stays_poisoned() {
    let mut day = Day::new();
    day.rig.fail_journal_write(1);
    let error = day.apply(&day.vector(-20, -30)).unwrap_err();
    assert!(
        matches!(error, DayWriteError::Write(WriteFailure::Journal(_))),
        "{error:?}"
    );
    day.assert_untouched();

    let again = day.apply(&day.vector(-20, -30)).unwrap_err();
    assert!(
        matches!(again, DayWriteError::Write(WriteFailure::Journal(_))),
        "{again:?}"
    );
    day.assert_untouched();
}

#[test]
fn a_readback_that_disagrees_is_no_success() {
    let mut day = Day::new();
    day.rig.sim.corrupt_readback(core(0));
    let error = day.apply(&day.vector(-20, -30)).unwrap_err();
    let DayWriteError::Write(WriteFailure::Mismatch {
        core: bad,
        expected,
        ..
    }) = error
    else {
        panic!("{error:?}");
    };
    assert_eq!((bad, expected), (core(0), TargetOffset::ZERO));
    assert_eq!(day.rig.kinds().last(), Some(&"smu.readback"));
    assert_eq!(
        day.rig
            .kinds()
            .iter()
            .filter(|kind| **kind == "smu.readback")
            .count(),
        1
    );
}

#[test]
fn a_failed_readback_is_recorded_and_is_no_success() {
    let mut day = Day::new();
    day.rig.sim.interrupt_smu(|operation: &SmuOperation| {
        if operation.op == SimOp::Read && operation.phase == HookPhase::Before {
            return Err(Error::Other("mailbox timeout".into()));
        }
        Ok(())
    });
    let error = day.apply(&day.vector(-20, -30)).unwrap_err();
    assert!(
        matches!(
            error,
            DayWriteError::Write(WriteFailure::Smu {
                op: SmuOp::Read,
                core: Some(_),
                ..
            })
        ),
        "{error:?}"
    );
    assert_eq!(
        day.rig.kinds(),
        ["session.start", "smu.intent", "smu.write", "smu.error"]
    );
}
