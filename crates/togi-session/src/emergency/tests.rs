use std::cell::Cell;
use std::rc::Rc;

use togi_journal::AppendError;
use togi_machine::{
    CoreId, CoreSet, Error, ObservedOffset, Smu, SmuValidated, Stopped, TargetOffset,
};
use togi_sim::faults::SmuOperation;

use super::{Access, Response, Withheld, respond};
use crate::error::JournalFailure;
use crate::testing::{Rig, core, target};
use crate::write::{Refusal, apply};

fn failure() -> JournalFailure {
    JournalFailure::new(AppendError::SeqExhausted)
}

const ORIGINAL: &str = "togi: journal write: the journal has no seq left";

/// An SMU that must not be touched.
struct Forbidden;

impl Smu for Forbidden {
    fn offset(&mut self, _: CoreId) -> Result<ObservedOffset, Error> {
        panic!("SMU read");
    }

    fn set_offset(&mut self, _: CoreId, _: TargetOffset) -> Result<(), Error> {
        panic!("SMU write");
    }

    fn set_all_offsets(&mut self, _: TargetOffset) -> Result<(), Error> {
        panic!("SMU all-core write");
    }

    fn emergency_zero(&mut self, _: SmuValidated, _: &Stopped) -> Result<(), Error> {
        panic!("SMU emergency zero");
    }
}

/// An SMU whose first emergency zeroing fails and leaves the offsets where they are.
struct FailsToZero {
    inner: Box<dyn Smu>,
}

impl Smu for FailsToZero {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error> {
        self.inner.offset(core)
    }

    #[expect(clippy::disallowed_methods, reason = "a test double forwards the seam")]
    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error> {
        self.inner.set_offset(core, offset)
    }

    #[expect(clippy::disallowed_methods, reason = "a test double forwards the seam")]
    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
        self.inner.set_all_offsets(offset)
    }

    fn emergency_zero(&mut self, _: SmuValidated, _: &Stopped) -> Result<(), Error> {
        Err(Error::from(
            Box::<dyn std::error::Error + Send + Sync>::from("simulated emergency zeroing failure"),
        ))
    }
}

fn text(report: Vec<u8>) -> String {
    String::from_utf8(report).unwrap()
}

fn cores(n: i64) -> CoreSet {
    (0..n).map(core).collect()
}

fn lower(rig: &mut Rig, n: i64, offset: i64) {
    let checked = rig
        .hardware
        .check_set(core(n), target(offset), &mut |_| None::<Refusal>)
        .unwrap();
    drop(apply(&mut rig.appender, &mut *rig.smu, &rig.stopped, checked, &[]).unwrap());
}

#[test]
fn nothing_touches_the_smu_without_a_sweep_a_validation_and_a_confirmed_teardown() {
    let rig = Rig::new(2);
    let cases = [
        (
            Access::gate(None, Some(rig.validated), Some(&rig.stopped)),
            Withheld::NoSweep,
            "offsets unchanged before successful trial-scope sweep",
        ),
        (
            Access::gate(Some(rig.sweep), None, Some(&rig.stopped)),
            Withheld::NoValidation,
            "offsets unchanged: the SMU interface was not validated",
        ),
        (
            Access::gate(Some(rig.sweep), Some(rig.validated), None),
            Withheld::TeardownUnconfirmed,
            "offsets unchanged: the stop of every workload is not confirmed",
        ),
        (
            Access::gate(None, None, None),
            Withheld::NoSweep,
            "offsets unchanged before successful trial-scope sweep",
        ),
    ];
    for (gate, withheld, said) in cases {
        let mut report = Vec::new();
        let outcome = respond(&failure(), gate, &mut Forbidden, cores(2), &mut report);
        assert!(matches!(outcome.response, Response::Withheld(held) if held == withheld));
        outcome.report.unwrap();
        assert_eq!(text(report), format!("{ORIGINAL}; {said}\n"));
    }
}

#[test]
fn a_gated_emergency_zeroes_every_core_reads_each_back_and_reports() {
    let mut rig = Rig::new(2);
    rig.start_at_zero();
    lower(&mut rig, 0, -10);
    lower(&mut rig, 1, -20);
    let events = rig.kinds();
    let mut report = Vec::new();
    let gate = Access::gate(Some(rig.sweep), Some(rig.validated), Some(&rig.stopped));
    let outcome = respond(&failure(), gate, &mut *rig.smu, cores(2), &mut report);
    let Response::Zeroed(zeroed) = &outcome.response else {
        panic!("withheld");
    };
    assert!(zeroed.is_clean());
    assert_eq!(zeroed.readbacks.len(), 2);
    assert_eq!((rig.offset(0), rig.offset(1)), (0, 0));
    assert_eq!(rig.kinds(), events);
    assert_eq!(
        text(report),
        format!("{ORIGINAL}; every core set to CO 0 without an intent (readback all 0)\n")
    );
}

#[test]
fn a_failed_zeroing_is_reported_with_what_every_core_still_reads() {
    let mut rig = Rig::new(2);
    rig.start_at_zero();
    lower(&mut rig, 0, -10);
    lower(&mut rig, 1, -20);
    let inner = std::mem::replace(&mut rig.smu, Box::new(Forbidden));
    let mut smu = FailsToZero { inner };
    let mut report = Vec::new();
    let gate = Access::gate(Some(rig.sweep), Some(rig.validated), Some(&rig.stopped));
    let outcome = respond(&failure(), gate, &mut smu, cores(2), &mut report);
    let Response::Zeroed(zeroed) = &outcome.response else {
        panic!("withheld");
    };
    assert!(zeroed.zero.is_err());
    assert!(!zeroed.is_clean());
    assert_eq!(
        text(report),
        format!(
            "{ORIGINAL}; setting every core to CO 0 without an intent failed: \
             simulated emergency zeroing failure (readback: core 00 reads -10, core 01 reads -20)\n"
        )
    );
}

#[test]
fn an_interruption_of_any_emergency_access_keeps_the_original_error_and_appends_nothing() {
    let accesses = 3 * 2;
    for interrupted in 1..=accesses {
        let mut rig = Rig::new(2);
        rig.start_at_zero();
        lower(&mut rig, 0, -10);
        let events = rig.kinds();
        let seen = Rc::new(Cell::new(0_u32));
        let counter = Rc::clone(&seen);
        rig.sim.interrupt_smu(move |_: &SmuOperation| {
            counter.set(counter.get() + 1);
            if counter.get() == interrupted {
                Err(Error::Crashed)
            } else {
                Ok(())
            }
        });
        let mut report = Vec::new();
        let gate = Access::gate(Some(rig.sweep), Some(rig.validated), Some(&rig.stopped));
        let outcome = respond(&failure(), gate, &mut *rig.smu, cores(2), &mut report);
        let Response::Zeroed(zeroed) = &outcome.response else {
            panic!("withheld at access {interrupted}");
        };
        assert!(zeroed.crashed(), "access {interrupted}");
        assert_eq!(seen.get(), interrupted, "access {interrupted}");
        assert!(
            text(report.clone()).contains("after the crash"),
            "access {interrupted}"
        );
        assert!(text(report).starts_with(ORIGINAL), "access {interrupted}");
        assert_eq!(rig.kinds(), events, "access {interrupted}");
    }
}

#[test]
fn an_unwritable_report_is_returned_not_lost() {
    struct Closed;
    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let rig = Rig::new(2);
    let gate = Access::gate(None, Some(rig.validated), Some(&rig.stopped));
    let outcome = respond(&failure(), gate, &mut Forbidden, cores(2), &mut Closed);
    assert!(outcome.report.is_err());
}
