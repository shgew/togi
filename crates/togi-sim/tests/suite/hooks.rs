// The SMU hook sees every actual access before and after its effect, and can fail either.
// Go: smu_interrupt_test.go (TestSMUInterruptionSurroundsActualEffect).

use std::cell::RefCell;
use std::rc::Rc;
use togi_machine::Error;
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};

use crate::common::{Rig, core, machine};

fn rig() -> Rig {
    let mut definition = machine(2, vec![]);
    definition.bios = Some(vec![-5, -6]);
    Rig::new(&definition, 1)
}

fn registers(rig: &mut Rig) -> Vec<i64> {
    // A read that no hook sees: take the hook out first.
    (0..2)
        .map(|index| rig.read(index).expect("read").0)
        .collect()
}

#[test]
fn a_hook_failing_before_an_access_stops_it_and_one_failing_after_cannot_undo_it() {
    for op in [SmuOp::Set, SmuOp::SetAll, SmuOp::Read] {
        for phase in [HookPhase::Before, HookPhase::After] {
            let mut rig = rig();
            if op == SmuOp::Read {
                rig.sim.corrupt_readback(core(0));
                rig.set(0, -5).expect("arm the corruption by writing");
            }
            let fired = Rc::new(RefCell::new(false));
            let seen = fired.clone();
            rig.sim.interrupt_smu(move |access: &SmuOperation| {
                if access.op == op && access.phase == phase {
                    *seen.borrow_mut() = true;
                    return Err(Error::Other("interrupted".into()));
                }
                Ok(())
            });
            let result = match op {
                SmuOp::Set => rig.set(0, -17),
                SmuOp::SetAll => rig.set_all(-17),
                SmuOp::Read => rig.read(0).map(drop),
            };
            assert!(*fired.borrow(), "{op:?} {phase:?}");
            assert!(matches!(result, Err(Error::Other(_))), "{op:?} {phase:?}");
            rig.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
            let after = phase == HookPhase::After;
            let want = match (op, after) {
                (SmuOp::Set, true) => vec![-17, -6],
                (SmuOp::SetAll, true) => vec![-17, -17],
                _ => vec![-5, -6],
            };
            let corrupted = rig.read(0).expect("read").0;
            if op == SmuOp::Read {
                // The corrupted observation is consumed only when the access happened.
                assert_eq!(corrupted, if after { -5 } else { -6 }, "{op:?} {phase:?}");
            } else {
                assert_eq!(registers(&mut rig), want, "{op:?} {phase:?}");
            }
        }
    }
}

#[test]
fn a_write_transaction_is_seen_before_and_after_with_its_core_and_offset() {
    let mut rig = rig();
    let log = Rc::new(RefCell::new(Vec::new()));
    let seen = log.clone();
    rig.sim.interrupt_smu(move |access: &SmuOperation| {
        seen.borrow_mut().push(*access);
        Ok(())
    });
    rig.set(1, -9).expect("set");
    rig.set_all(-3).expect("set all");
    rig.read(1).expect("read");
    let ops: Vec<_> = log.borrow().clone();
    let at = |op, core, offset, phase| SmuOperation {
        op,
        core,
        offset,
        phase,
    };
    assert_eq!(
        ops,
        [
            at(SmuOp::Set, Some(core(1)), -9, HookPhase::Before),
            at(SmuOp::Set, Some(core(1)), -9, HookPhase::After),
            at(SmuOp::SetAll, None, -3, HookPhase::Before),
            at(SmuOp::SetAll, None, -3, HookPhase::After),
            at(SmuOp::Read, Some(core(1)), -3, HookPhase::Before),
            at(SmuOp::Read, Some(core(1)), -3, HookPhase::After),
        ]
    );
}

#[test]
fn an_access_that_does_not_happen_is_not_reported_to_the_hook() {
    let mut rig = rig();
    let log = Rc::new(RefCell::new(0));
    let seen = log.clone();
    rig.sim.interrupt_smu(move |_: &SmuOperation| {
        *seen.borrow_mut() += 1;
        Ok(())
    });
    rig.sim.fail_write(1);
    assert!(rig.set(0, -1).is_err());
    assert!(rig.machine.smu.offset(core(2)).is_err());
    rig.sim.crash();
    assert!(rig.set_all(-1).is_err());
    assert_eq!(*log.borrow(), 0);
}

#[test]
fn a_hook_may_use_the_simulator_it_is_called_from() {
    let mut rig = rig();
    let sim = rig.sim.clone();
    rig.sim.interrupt_smu(move |access: &SmuOperation| {
        if access.phase == HookPhase::After {
            sim.fail_check("root", "set from a hook");
            sim.escape();
        }
        Ok(())
    });
    rig.set(0, -2).expect("set");
    assert_eq!(rig.machine.host.preflight()[0].detail, "set from a hook");
    rig.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
}

#[test]
fn the_emergency_zero_writes_every_core_through_the_hook_and_the_write_faults() {
    let mut rig = rig();
    let log = Rc::new(RefCell::new(Vec::new()));
    let seen = log.clone();
    rig.sim.interrupt_smu(move |access: &SmuOperation| {
        seen.borrow_mut()
            .push((access.op, access.offset, access.phase));
        Ok(())
    });
    let validated = rig.machine.host.validate_smu().expect("validated");
    let stopped = rig.proof();
    rig.machine
        .smu
        .emergency_zero(validated, &stopped)
        .expect("zeroed");
    assert_eq!(
        *log.borrow(),
        [
            (SmuOp::SetAll, 0, HookPhase::Before),
            (SmuOp::SetAll, 0, HookPhase::After)
        ]
    );
    assert_eq!(registers(&mut rig), [0, 0]);
    rig.sim.fail_write(1);
    assert!(rig.machine.smu.emergency_zero(validated, &stopped).is_err());
}
