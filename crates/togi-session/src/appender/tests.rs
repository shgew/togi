use std::cell::RefCell;
use std::rc::Rc;

use jiff::tz::TimeZone;
use togi_journal::{ApplicationId, ApplyCause, CcdVector, DayApplied, Payload, SessionBaseline};
use togi_machine::{BootId, CcdId, CcdProfile, CoreInfo, ObservedOffset, PerCore};
use togi_render::Renderer;

use super::Appender;
use crate::error::{AppendFailure, FoldError, JournalCause, JournalFailure};
use crate::testing::{Rig, assert_fold_consistent, core};

/// A day application naming other CCDs than the baseline: the policy cannot fold it.
fn unscheduled() -> Payload {
    let ccd = CcdId::new(9).unwrap();
    let topology = [CoreInfo {
        core: core(0),
        ccd,
        cpus: Vec::new(),
    }];
    Payload::DayApplied(DayApplied {
        application: ApplicationId(1),
        cause: ApplyCause::Reapplication {},
        target: CcdVector::expand(CcdProfile::zero([ccd]).unwrap(), &topology).unwrap(),
        readbacks: PerCore::default(),
        transactions: Vec::new(),
        boot: BootId::from("boot".to_owned()),
        mono_ns: 0,
    })
}

/// A rig whose journal holds the verified all-zero baseline the day history starts from.
fn baselined() -> Rig {
    let mut rig = Rig::new(2);
    let offsets = (0..2)
        .map(|index| (core(index), ObservedOffset(0)))
        .collect();
    rig.appender
        .append(Payload::SessionBaseline(SessionBaseline { offsets }), &[])
        .unwrap();
    rig
}

#[test]
fn every_append_folds_into_the_recovery_the_tuner_and_the_defect_state() {
    let mut rig = Rig::new(2);
    rig.start_at_zero();
    assert_fold_consistent(&rig.appender);
}

#[test]
fn the_append_hook_sees_every_event_once_in_order() {
    let mut rig = Rig::new(2);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    rig.appender.on_append = Some(Box::new(move |event| sink.borrow_mut().push(event.clone())));
    let before = rig.events().len();
    rig.start_at_zero();
    let appended = &rig.events()[before..];
    assert!(!appended.is_empty());
    assert_eq!(*seen.borrow(), appended);
}

#[test]
fn an_event_the_tuner_never_planned_is_a_corrupt_journal_now_and_on_resume() {
    let mut rig = baselined();
    let failure = rig.appender.append(unscheduled(), &[]).unwrap_err();
    let AppendFailure::Fold(FoldError::Tuner { seq, .. }) = failure else {
        panic!("{failure:?}");
    };
    assert_eq!(rig.events().last().map(|event| event.seq), Some(seq));
    let clock = rig.sim.seams().clock;
    let resumed = Appender::resume(
        rig.appender.into_journal(),
        clock,
        Renderer::default(),
        TimeZone::UTC,
    );
    assert!(matches!(
        resumed,
        Err(FoldError::Tuner { seq: at, .. }) if at == seq
    ));
}

#[test]
fn an_event_the_run_cannot_fold_latches_the_appender_and_cancels_the_trial() {
    let mut rig = baselined();
    let token = togi_machine::CancellationToken::new();
    rig.appender.watch_trial(Some(token.clone()));
    let before = rig.events().len();
    assert!(matches!(
        rig.appender.append(unscheduled(), &[]),
        Err(AppendFailure::Fold(_))
    ));
    assert!(matches!(
        rig.appender.failure().map(JournalFailure::cause),
        Some(JournalCause::Fold(_))
    ));
    assert!(token.is_cancelled());
    assert!(matches!(
        rig.appender.append(unscheduled(), &[]),
        Err(AppendFailure::Journal(_))
    ));
    assert_eq!(rig.events().len(), before + 1);
}

/// A panic out of the log line, the append hook or the folds leaves a durable event the run's state may not have:
/// the appender latches, cancels the trial and refuses everything after, and the panic still unwinds.
#[test]
fn a_panic_taking_in_a_durable_event_latches_the_appender_and_still_unwinds() {
    let mut rig = baselined();
    rig.appender.on_append = Some(Box::new(|_| panic!("a bug in the hook")));
    let token = togi_machine::CancellationToken::new();
    rig.appender.watch_trial(Some(token.clone()));
    let before = rig.events().len();
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(rig.appender.append(unscheduled(), &[]));
    }))
    .unwrap_err();
    assert_eq!(
        crate::session::message_of(&*payload),
        "a bug in the hook",
        "the original panic unwinds"
    );
    assert!(matches!(
        rig.appender.failure().map(JournalFailure::cause),
        Some(JournalCause::Interrupted(message)) if message == "a bug in the hook"
    ));
    assert!(token.is_cancelled());
    assert!(matches!(
        rig.appender.append(unscheduled(), &[]),
        Err(AppendFailure::Journal(_))
    ));
    assert_eq!(
        rig.events().len(),
        before + 1,
        "the event is durable, nothing after it"
    );
}

struct MalformedClock;

impl togi_machine::Clock for MalformedClock {
    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }

    fn monotonic(&self) -> Result<std::time::Duration, togi_machine::Error> {
        togi_machine::monotonic_duration(-1, 0)
    }

    fn sleep(
        &mut self,
        _: &togi_machine::CancellationToken,
        _: std::time::Duration,
    ) -> Result<(), togi_machine::Error> {
        Ok(())
    }
}

#[test]
fn a_malformed_clock_latches_before_appending_and_cancels_the_trial() {
    let mut rig = baselined();
    rig.appender.clock = Box::new(MalformedClock);
    let token = togi_machine::CancellationToken::new();
    rig.appender.watch_trial(Some(token.clone()));
    let before = rig.events().len();
    assert!(matches!(
        rig.appender.append(unscheduled(), &[]),
        Err(AppendFailure::Journal(_))
    ));
    assert!(matches!(
        rig.appender.failure().map(JournalFailure::cause),
        Some(JournalCause::Clock(
            togi_machine::Error::InvalidMonotonic { .. }
        ))
    ));
    assert!(token.is_cancelled());
    assert!(matches!(
        rig.appender.append(unscheduled(), &[]),
        Err(AppendFailure::Journal(_))
    ));
    assert_eq!(rig.events().len(), before);
}
