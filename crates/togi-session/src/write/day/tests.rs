use std::collections::BTreeMap;

use serde_json::json;
use test_case::test_case;

use togi_journal::{CcdVector, Event, Payload, SmuCommand, SmuOp};
use togi_machine::{
    CcdId, CcdProfile, CcdProfileError, Clock, CoreInfo, Error, Machine, ObservedOffset, PerCore,
    Smu, TargetOffset,
};
use togi_sim::MachineDefinition;
use togi_sim::faults::{HookPhase, SmuOp as SimOp, SmuOperation};

use super::{DayAppliedProfile, DayRecovery, DayWriteError, apply_day_profile};
use crate::appender::Folds;
use crate::testing::{Rig, core, target};
use crate::write::{ProfileFailure, WriteFailure};

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

    fn apply(&mut self, requested: &CcdVector) -> Result<DayAppliedProfile, DayWriteError> {
        apply_day_profile(
            &mut self.rig.appender.journal,
            &*self.clock,
            &mut *self.rig.smu,
            self.rig.validated,
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

fn zero_all() -> SmuCommand {
    SmuCommand::SetAll {
        offset: TargetOffset::ZERO,
    }
}

fn set(n: i64, offset: i64) -> SmuCommand {
    SmuCommand::Set {
        core: core(n),
        offset: target(offset),
    }
}

/// The kinds of a transaction on four cores: the zeroing, `sets` single-core writes, then `confirmed` readbacks.
fn kinds(sets: usize, confirmed: usize) -> Vec<&'static str> {
    let mut kinds = vec!["session.start", "smu.intent", "smu.write"];
    kinds.extend(["smu.readback"; 4]);
    for _ in 0..sets {
        kinds.extend(["smu.intent", "smu.write", "smu.readback"]);
    }
    kinds.extend(vec!["smu.readback"; confirmed]);
    kinds
}

fn readback(event: &Event) -> (i64, Option<i64>) {
    let Payload::SmuReadback(record) = &event.payload else {
        panic!("{:?}", event.payload);
    };
    (record.offset.0, record.expected.map(TargetOffset::get))
}

fn failure(error: DayWriteError) -> (ProfileFailure, DayRecovery) {
    match error {
        DayWriteError::Write { failure, recovery } => (failure, *recovery),
        other @ (DayWriteError::Profile(_) | DayWriteError::Expansion { .. }) => {
            panic!("{other:?}")
        }
    }
}

#[test]
fn a_nonzero_request_zeroes_then_writes_each_core_then_reads_every_core() {
    let mut day = Day::new();
    let requested = day.vector(-20, -30);
    let applied = day.apply(&requested).unwrap();

    assert_eq!(applied.target, requested);
    assert_eq!(
        applied.readbacks,
        (0..4)
            .zip([-20, -20, -30, -30])
            .map(|(n, offset)| (core(n), ObservedOffset(offset)))
            .collect::<PerCore<_>>()
    );
    assert_eq!(day.offsets(), [-20, -20, -30, -30]);
    assert_eq!(
        day.commands(),
        [
            zero_all(),
            set(0, -20),
            set(1, -20),
            set(2, -30),
            set(3, -30)
        ]
    );
    assert_eq!(day.rig.kinds(), kinds(4, 4));

    let events = day.rig.events();
    let chain: Vec<_> = events[1..].iter().map(|event| event.seq).collect();
    assert_eq!(applied.transactions, chain);

    // The zeroing: the write cites its intent and each readback the write.
    assert_eq!(events[2].cause, [events[1].seq]);
    for event in &events[3..7] {
        assert_eq!(event.cause, [events[2].seq]);
        assert_eq!(readback(event), (0, Some(0)));
    }
    // Each core: its own intent, a write citing it and a readback of the target citing the write.
    for (n, offset) in [-20, -20, -30, -30].into_iter().enumerate() {
        let base = 7 + 3 * n;
        assert_eq!(events[base + 1].cause, [events[base].seq]);
        assert_eq!(events[base + 2].cause, [events[base + 1].seq]);
        assert_eq!(readback(&events[base + 2]), (offset, Some(offset)));
    }
    // The final pass reads every core against its target, citing the last write.
    for (event, offset) in events[19..].iter().zip([-20, -20, -30, -30]) {
        assert_eq!(event.cause, [events[17].seq]);
        assert_eq!(readback(event), (offset, Some(offset)));
    }
    Folds::replay(events).unwrap();
}

#[test]
fn only_the_nonzero_cores_are_written_one_by_one() {
    let mut day = Day::new();
    let requested = day.vector(0, -30);
    let applied = day.apply(&requested).unwrap();

    assert_eq!(applied.target, requested);
    assert_eq!(day.offsets(), [0, 0, -30, -30]);
    assert_eq!(day.commands(), [zero_all(), set(2, -30), set(3, -30)]);
    assert_eq!(day.rig.kinds(), kinds(2, 4));
    let events = day.rig.events();
    for (event, offset) in events[13..].iter().zip([0, 0, -30, -30]) {
        assert_eq!(readback(event), (offset, Some(offset)));
    }
}

#[test]
fn a_stock_request_is_one_zero_write_and_four_readbacks() {
    let mut day = Day::new();
    let requested = day.vector(0, 0);
    let applied = day.apply(&requested).unwrap();
    assert_eq!(applied.target, requested);
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
    assert_eq!(day.commands(), [zero_all()]);
    assert_eq!(day.rig.kinds(), kinds(0, 0));
    assert_eq!(applied.transactions.len(), 6);
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
fn a_malformed_vector_is_refused_before_anything_is_journaled_or_written(tamper: Tamper, bad: i64) {
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
fn a_profile_that_does_not_cover_the_topology_is_refused_before_anything_is_journaled_or_written() {
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
    let error = apply_day_profile(
        &mut day.rig.appender.journal,
        &*day.clock,
        &mut *day.rig.smu,
        day.rig.validated,
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
fn a_failed_first_intent_zeroes_without_a_journal_and_the_journal_stays_poisoned() {
    let mut day = Day::new();
    day.rig.fail_journal_write(1);
    let error = day.apply(&day.vector(-20, -30)).unwrap_err();
    let (failed, recovery) = failure(error);
    assert!(
        matches!(failed, ProfileFailure::Write(WriteFailure::Journal(_))),
        "{failed:?}"
    );
    let DayRecovery::Emergency(zeroed) = recovery else {
        panic!("{recovery:?}");
    };
    assert!(zeroed.is_clean(), "{zeroed:?}");
    // Nothing was journaled, yet every core is at 0: the emergency wrote no intent.
    assert_eq!(day.rig.kinds(), ["session.start"]);
    assert_eq!(day.offsets(), [0, 0, 0, 0]);

    let (again, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    assert!(
        matches!(again, ProfileFailure::Write(WriteFailure::Journal(_))),
        "{again:?}"
    );
    assert!(
        matches!(recovery, DayRecovery::Emergency(_)),
        "{recovery:?}"
    );
    assert_eq!(day.rig.kinds(), ["session.start"]);
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
}

#[test]
fn a_nonzero_write_whose_record_cannot_be_appended_leaves_its_intent_and_is_zeroed() {
    let mut day = Day::new();
    // Appends 1 to 6 are the zeroing; 7 is core 0's intent and 8 the record of its write, which cannot be made
    // durable once the journal is poisoned although the SMU already took the command.
    day.rig.fail_journal_write(8);
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    assert!(
        matches!(failed, ProfileFailure::Write(WriteFailure::Journal(_))),
        "{failed:?}"
    );
    let DayRecovery::Emergency(zeroed) = recovery else {
        panic!("{recovery:?}");
    };
    assert!(zeroed.is_clean(), "{zeroed:?}");
    assert_eq!(day.commands(), [zero_all(), set(0, -20)]);
    let mut expected = kinds(0, 0);
    expected.push("smu.intent");
    assert_eq!(day.rig.kinds(), expected);
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
}

#[test]
fn a_zero_readback_that_disagrees_is_no_success_and_is_restored() {
    let mut day = Day::new();
    day.rig.sim.corrupt_readback(core(0));
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    let ProfileFailure::Write(WriteFailure::Mismatch {
        core: bad,
        expected,
        ..
    }) = failed
    else {
        panic!("{failed:?}");
    };
    assert_eq!((bad, expected), (core(0), TargetOffset::ZERO));
    assert!(matches!(recovery, DayRecovery::Restored), "{recovery:?}");
    assert_eq!(
        day.rig.kinds(),
        [
            "session.start",
            "smu.intent",
            "smu.write",
            "smu.readback",
            "smu.intent",
            "smu.write",
            "smu.readback",
            "smu.readback",
            "smu.readback",
            "smu.readback",
        ]
    );
    assert_eq!(day.commands(), [zero_all(), zero_all()]);
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
}

#[test]
fn a_nonzero_readback_that_disagrees_is_durable_no_success_and_restored() {
    let mut day = Day::new();
    // Once the zeroing is written, corrupt the readback of core 2's own write.
    let sim = day.rig.sim.clone();
    let mut armed = false;
    day.rig.sim.interrupt_smu(move |operation: &SmuOperation| {
        if !armed && operation.op == SimOp::SetAll && operation.phase == HookPhase::After {
            armed = true;
            sim.corrupt_readback(core(2));
        }
        Ok(())
    });
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    let ProfileFailure::Write(WriteFailure::Mismatch {
        core: bad,
        observed,
        expected,
    }) = failed
    else {
        panic!("{failed:?}");
    };
    assert_eq!(
        (bad, observed, expected),
        (core(2), ObservedOffset(-31), target(-30))
    );
    assert!(matches!(recovery, DayRecovery::Restored), "{recovery:?}");
    assert_eq!(
        day.commands(),
        [
            zero_all(),
            set(0, -20),
            set(1, -20),
            set(2, -30),
            zero_all()
        ]
    );
    // The disagreeing readback is journaled before the restoring transaction begins.
    let events = day.rig.events();
    assert_eq!(readback(&events[15]), (-31, Some(-30)));
    assert_eq!(day.rig.kinds()[16], "smu.intent");
    Folds::replay(events).unwrap();
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
}

/// Writes core 0 behind the day's back, as firmware could.
#[expect(
    clippy::disallowed_methods,
    reason = "the test plays firmware changing an offset; only the write transaction writes for togi"
)]
fn clobber(smu: &mut dyn Smu) {
    smu.set_offset(core(0), target(-1)).unwrap();
}

#[test]
fn a_core_that_changes_before_the_final_readback_is_no_success_and_is_restored() {
    let mut day = Day::new();
    let Machine { mut smu, .. } = day.rig.sim.seams();
    day.rig.sim.interrupt_smu(move |operation: &SmuOperation| {
        if operation.op == SimOp::Set
            && operation.core == Some(core(3))
            && operation.phase == HookPhase::After
        {
            clobber(&mut *smu);
        }
        Ok(())
    });
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    let ProfileFailure::Write(WriteFailure::Mismatch {
        core: bad,
        observed,
        expected,
    }) = failed
    else {
        panic!("{failed:?}");
    };
    assert_eq!(
        (bad, observed, expected),
        (core(0), ObservedOffset(-1), target(-20))
    );
    assert!(matches!(recovery, DayRecovery::Restored), "{recovery:?}");
    // Only the first of the final readbacks was made: the disagreement ended the pass.
    let events = day.rig.events();
    assert_eq!(readback(&events[19]), (-1, Some(-20)));
    assert_eq!(day.rig.kinds()[20], "smu.intent");
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
}

#[test]
fn a_failed_write_midway_is_recorded_after_its_durable_intent_and_restored() {
    let mut day = Day::new();
    day.rig.sim.interrupt_smu(|operation: &SmuOperation| {
        if operation.op == SimOp::Set
            && operation.core == Some(core(2))
            && operation.phase == HookPhase::Before
        {
            return Err(Error::Other("mailbox timeout".into()));
        }
        Ok(())
    });
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    assert!(
        matches!(
            failed,
            ProfileFailure::Write(WriteFailure::Smu {
                op: SmuOp::Set,
                core: Some(bad),
                ..
            }) if bad == core(2)
        ),
        "{failed:?}"
    );
    assert!(matches!(recovery, DayRecovery::Restored), "{recovery:?}");
    // Cores 0 and 1 were written, core 2's intent was durable before the SMU refused it and the error is recorded.
    assert_eq!(
        day.commands(),
        [
            zero_all(),
            set(0, -20),
            set(1, -20),
            set(2, -30),
            zero_all()
        ]
    );
    let mut expected = kinds(2, 0);
    expected.extend(["smu.intent", "smu.error"]);
    expected.extend(["smu.intent", "smu.write"]);
    expected.extend(["smu.readback"; 4]);
    assert_eq!(day.rig.kinds(), expected);
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
    Folds::replay(day.rig.events()).unwrap();
}

#[test]
fn a_restoration_that_fails_is_reported_and_leaves_the_offsets_unconfirmed() {
    let mut day = Day::new();
    let mut zeroings = 0;
    day.rig.sim.interrupt_smu(move |operation: &SmuOperation| {
        if operation.phase == HookPhase::Before {
            if operation.op == SimOp::SetAll {
                zeroings += 1;
                if zeroings == 2 {
                    return Err(Error::Other("mailbox timeout".into()));
                }
            }
            if operation.op == SimOp::Set && operation.core == Some(core(2)) {
                return Err(Error::Other("mailbox timeout".into()));
            }
        }
        Ok(())
    });
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    assert!(
        matches!(
            failed,
            ProfileFailure::Write(WriteFailure::Smu { op: SmuOp::Set, .. })
        ),
        "{failed:?}"
    );
    assert!(
        matches!(
            recovery,
            DayRecovery::Unrestored(WriteFailure::Smu {
                op: SmuOp::SetAll,
                core: None,
                ..
            })
        ),
        "{recovery:?}"
    );
    assert_eq!(day.rig.kinds().last(), Some(&"smu.error"));
    assert_eq!(day.offsets(), [-20, -20, 0, 0]);
}

#[test]
fn a_failed_final_readback_is_recorded_and_restored() {
    let mut day = Day::new();
    let mut reads = 0;
    day.rig.sim.interrupt_smu(move |operation: &SmuOperation| {
        if operation.op == SimOp::Read && operation.phase == HookPhase::Before {
            reads += 1;
            // Four readbacks of the zeroing and four after the writes come first.
            if reads == 9 {
                return Err(Error::Other("mailbox timeout".into()));
            }
        }
        Ok(())
    });
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    assert!(
        matches!(
            failed,
            ProfileFailure::Write(WriteFailure::Smu {
                op: SmuOp::Read,
                core: Some(bad),
                ..
            }) if bad == core(0)
        ),
        "{failed:?}"
    );
    assert!(matches!(recovery, DayRecovery::Restored), "{recovery:?}");
    assert_eq!(day.rig.kinds()[19], "smu.error");
    assert_eq!(day.offsets(), [0, 0, 0, 0]);
}

#[test]
fn a_crash_ends_all_smu_access_and_nothing_is_restored() {
    let mut day = Day::new();
    day.rig.crash_at(SimOp::Set, HookPhase::Before);
    let (failed, recovery) = failure(day.apply(&day.vector(-20, -30)).unwrap_err());
    assert!(
        matches!(failed, ProfileFailure::Write(WriteFailure::Crashed)),
        "{failed:?}"
    );
    assert!(matches!(recovery, DayRecovery::Crashed), "{recovery:?}");
    // The durable intent stays and nothing follows it: no error record, no restoring transaction.
    let mut expected = kinds(0, 0);
    expected.push("smu.intent");
    assert_eq!(day.rig.kinds(), expected);
    assert_eq!(day.commands(), [zero_all(), set(0, -20)]);
}
