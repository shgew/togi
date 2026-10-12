use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::rc::Rc;

use jiff::tz::TimeZone;
use togi_journal::fs::{Fault, Op};
use togi_journal::{
    ArchiveKind, DeadEnd, DeadEndAction, DeadEndCondition, EventKind, Payload, SessionStart,
    SessionWarning, Stamp,
};
use togi_render::Renderer;
use togi_tuningboot::{
    COUNT_VARIABLE, Environment, LeaveReason, REASON_VARIABLE, RestartCount, SavedEntry,
};

use super::{BootError, BootHandoff};
use crate::appender::Appender;
use crate::error::{AppendFailure, SessionError};
use crate::testing::{Bench, Rig, build};

#[derive(Default)]
struct Env {
    values: BTreeMap<String, String>,
    changes: Vec<String>,
    refuse: BTreeSet<String>,
    unreadable: BTreeSet<String>,
    entries_cleared: u32,
}

#[derive(Clone, Default)]
struct Grub(Rc<RefCell<Env>>);

impl Grub {
    fn with_count(count: &str) -> Self {
        let grub = Self::default();
        grub.0
            .borrow_mut()
            .values
            .insert(COUNT_VARIABLE.to_owned(), count.to_owned());
        grub
    }

    fn pending(&self, reason: &LeaveReason) {
        togi_tuningboot::write_reason(&mut self.clone(), reason).unwrap();
        self.0.borrow_mut().changes.clear();
    }

    fn handoff(&self) -> Result<BootHandoff, BootError> {
        BootHandoff::read(Box::new(self.clone()))
    }

    fn changes(&self) -> Vec<String> {
        self.0.borrow().changes.clone()
    }

    fn value(&self, name: &str) -> Option<String> {
        self.0.borrow().values.get(name).cloned()
    }

    fn refuse(&self, name: &str) {
        self.0.borrow_mut().refuse.insert(name.to_owned());
    }

    fn allow(&self) {
        self.0.borrow_mut().refuse.clear();
    }
}

fn failure(name: &str) -> io::Error {
    io::Error::other(format!("cannot change {name}"))
}

impl Environment for Grub {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        let env = self.0.borrow();
        if env.unreadable.contains(name) {
            return Err(failure(name));
        }
        Ok(env.values.get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        let mut env = self.0.borrow_mut();
        if let Some((name, _)) = values.iter().find(|(name, _)| env.refuse.contains(*name)) {
            return Err(failure(name));
        }
        for (name, value) in values {
            env.changes.push(format!("set:{name}"));
            env.values.insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        let mut env = self.0.borrow_mut();
        if let Some(name) = names.iter().find(|name| env.refuse.contains(**name)) {
            return Err(failure(name));
        }
        for name in names {
            env.changes.push(format!("unset:{name}"));
            env.values.remove(*name);
        }
        Ok(())
    }
}

impl togi_tuningboot::Bootloader for Grub {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        self.0.borrow_mut().entries_cleared += 1;
        Ok(SavedEntry {
            before: "tuning".into(),
            after: "normal".into(),
        })
    }
}

fn exhausted() -> LeaveReason {
    LeaveReason::new(
        "service restart limit exhausted; returning to the normal system",
        RestartCount::try_from(3).unwrap(),
    )
    .unwrap()
}

fn leave_reasons(rig: &Rig) -> Vec<String> {
    rig.events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::BootLeaveReason(leave) => Some(leave.reason_id.clone()),
            _ => None,
        })
        .collect()
}

fn warning(appender: &mut Appender) -> Result<togi_journal::Seq, AppendFailure> {
    appender.append(
        Payload::SessionWarning(SessionWarning {
            operation: "progress".into(),
            trial: None,
            error: String::new(),
        }),
        &[],
    )
}

/// Reopens the journal of a finished rig for a later run of the same boot, with its own handoff.
fn resume(rig: Rig, handoff: BootHandoff) -> Appender {
    let sim = rig.sim.clone();
    let dir = rig.appender.into_journal().close().unwrap();
    let journal = dir.open_at(rig.boot.clone(), sim.now()).unwrap();
    Appender::resume(
        journal,
        sim.seams().clock,
        Renderer::default(),
        TimeZone::UTC,
    )
    .unwrap()
    .with_handoff(handoff)
}

#[test]
fn the_restart_count_resets_once_and_only_after_the_first_durable_append() {
    let grub = Grub::with_count("2");
    let handoff = grub.handoff().unwrap();
    assert!(grub.changes().is_empty());
    let (mut rig, first) = Rig::custom(2, Some(handoff), |_| {});
    first.unwrap();
    assert_eq!(grub.changes(), [format!("unset:{COUNT_VARIABLE}")]);
    assert_eq!(grub.value(COUNT_VARIABLE), None);
    warning(&mut rig.appender).unwrap();
    assert_eq!(grub.changes().len(), 1);
}

#[test]
fn a_failed_first_append_leaves_the_count_and_the_reason_alone() {
    let grub = Grub::with_count("2");
    grub.pending(&exhausted());
    let handoff = grub.handoff().unwrap();
    let (rig, first) = Rig::custom(2, Some(handoff), |mem| {
        mem.inject(togi_journal::fs::Fault::before(
            togi_journal::fs::Op::Write,
            1,
        ));
    });
    assert!(matches!(first, Err(AppendFailure::Journal(_))));
    assert!(rig.appender.failure().is_some());
    assert!(grub.changes().is_empty());
    assert_eq!(grub.value(COUNT_VARIABLE).as_deref(), Some("2"));
    assert!(grub.value(REASON_VARIABLE).is_some());
}

#[test]
fn a_reason_that_cannot_be_read_stops_before_anything_is_appended() {
    let grub = Grub::with_count("2");
    grub.0
        .borrow_mut()
        .unreadable
        .insert(REASON_VARIABLE.to_owned());
    let error = grub.handoff().err().unwrap();
    assert!(matches!(
        error,
        BootError::Bootloader(togi_tuningboot::Error::ReadReason(_))
    ));
    assert!(grub.changes().is_empty());
}

#[test]
fn the_pending_reason_is_journaled_then_cleared() {
    let grub = Grub::default();
    let reason = exhausted();
    grub.pending(&reason);
    let (rig, first) = Rig::custom(2, Some(grub.handoff().unwrap()), |_| {});
    first.unwrap();
    assert_eq!(rig.kinds(), ["session.start", "boot.leave_reason"]);
    assert_eq!(leave_reasons(&rig), [reason.id.to_string()]);
    let Payload::BootLeaveReason(leave) = &rig.events()[1].payload else {
        panic!("not a leave reason");
    };
    assert_eq!(
        (leave.restart_limit_count, leave.reason.as_str()),
        (3, reason.reason.as_str())
    );
    assert_eq!(grub.value(REASON_VARIABLE), None);
    let leave_at = grub
        .changes()
        .iter()
        .position(|change| change == &format!("unset:{REASON_VARIABLE}"))
        .unwrap();
    assert_eq!(leave_at, 1);
}

#[test]
fn a_reason_whose_event_failed_stays_pending_and_is_imported_by_the_next_run() {
    let grub = Grub::default();
    let reason = exhausted();
    grub.pending(&reason);
    let (rig, first) = Rig::custom(2, Some(grub.handoff().unwrap()), |mem| {
        mem.inject(togi_journal::fs::Fault::before(
            togi_journal::fs::Op::Write,
            2,
        ));
    });
    assert!(matches!(first, Err(AppendFailure::Journal(_))));
    assert_eq!(rig.kinds(), ["session.start"]);
    assert!(grub.value(REASON_VARIABLE).is_some());

    let mut next = resume(rig, grub.handoff().unwrap());
    warning(&mut next).unwrap();
    let ids: Vec<_> = next
        .journal()
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::BootLeaveReason(leave) => Some(leave.reason_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, [reason.id.to_string()]);
    assert_eq!(grub.value(REASON_VARIABLE), None);
}

#[test]
fn a_reason_durable_before_its_clearing_failed_is_not_journaled_twice() {
    let grub = Grub::default();
    let reason = exhausted();
    grub.pending(&reason);
    grub.refuse(REASON_VARIABLE);
    let (rig, first) = Rig::custom(2, Some(grub.handoff().unwrap()), |_| {});
    assert!(matches!(
        first,
        Err(AppendFailure::Boot(BootError::Bootloader(
            togi_tuningboot::Error::ClearReason(_)
        )))
    ));
    assert_eq!(rig.kinds(), ["session.start", "boot.leave_reason"]);
    assert!(grub.value(REASON_VARIABLE).is_some());

    grub.allow();
    let mut next = resume(rig, grub.handoff().unwrap());
    warning(&mut next).unwrap();
    let kinds: Vec<_> = next
        .journal()
        .events()
        .iter()
        .map(|e| e.kind().as_str())
        .collect();
    assert_eq!(
        kinds,
        ["session.start", "boot.leave_reason", "session.warning"]
    );
    assert_eq!(grub.value(REASON_VARIABLE), None);
}

#[test]
fn a_reason_an_archived_journal_holds_is_cleared_without_a_new_event() {
    let grub = Grub::default();
    let reason = exhausted();
    grub.pending(&reason);
    grub.refuse(REASON_VARIABLE);
    let (rig, first) = Rig::custom(2, Some(grub.handoff().unwrap()), |_| {});
    assert!(first.is_err());
    let sim = rig.sim.clone();
    let boot = rig.boot.clone();
    let stamp = Stamp {
        time: sim.now(),
        mono: None,
    };
    let dir = rig
        .appender
        .into_journal()
        .archive(stamp, ArchiveKind::Transition, &[])
        .unwrap();
    let journal = dir.open_at(boot, sim.now()).unwrap();
    grub.allow();
    let mut next = Appender::resume(
        journal,
        sim.seams().clock,
        Renderer::default(),
        TimeZone::UTC,
    )
    .unwrap()
    .with_handoff(grub.handoff().unwrap());
    next.append(
        Payload::SessionStart(SessionStart {
            build: build(),
            session: "20260101T000100Z".parse().unwrap(),
            cores: sim.seams().host.topology().unwrap(),
        }),
        &[],
    )
    .unwrap();
    let kinds: Vec<_> = next
        .journal()
        .events()
        .iter()
        .map(|e| e.kind().as_str())
        .collect();
    assert_eq!(kinds, ["session.start"]);
    assert_eq!(grub.value(REASON_VARIABLE), None);
}

#[test]
fn bootloader_failures_are_ordinary_errors_and_the_journal_keeps_working() {
    for name in [COUNT_VARIABLE, REASON_VARIABLE] {
        let grub = Grub::with_count("1");
        grub.pending(&exhausted());
        grub.refuse(name);
        let (mut rig, first) = Rig::custom(2, Some(grub.handoff().unwrap()), |_| {});
        assert!(matches!(first, Err(AppendFailure::Boot(_))), "{name}");
        assert!(rig.appender.failure().is_none(), "{name}");
        assert!(grub.value(REASON_VARIABLE).is_some(), "{name}");
        warning(&mut rig.appender).unwrap();
        assert!(rig.appender.failure().is_none(), "{name}");
    }
}

#[test]
fn leaving_for_a_dead_end_saves_a_bounded_reason_then_clears_the_saved_entry() {
    let grub = Grub::default();
    let mut handoff = grub.handoff().unwrap();
    let text = format!("dead end preflight: uid 1000\n{}", "x".repeat(300));
    let left = handoff.leave(&text);
    assert!(left.reason_error.is_none());
    let saved = togi_tuningboot::read_reason(&mut grub.clone())
        .unwrap()
        .unwrap();
    assert!(saved.reason.as_str().starts_with("dead end preflight:"));
    assert!(saved.reason.as_str().len() <= 160);
    assert!(!saved.reason.as_str().contains('\n'));
    assert_eq!(saved.count, RestartCount::ZERO);
    assert_eq!(grub.0.borrow().entries_cleared, 1);
    let payload = left.payload();
    assert_eq!(
        (
            payload.before.as_deref(),
            payload.after.as_deref(),
            payload.error,
            payload.reason_error
        ),
        (Some("tuning"), Some("normal"), None, None)
    );
}

#[test]
fn a_reason_that_cannot_be_saved_never_changes_whether_the_entry_was_cleared() {
    let grub = Grub::default();
    grub.refuse(REASON_VARIABLE);
    let left = grub.handoff().unwrap().leave("dead end");
    assert_eq!(grub.0.borrow().entries_cleared, 1);
    let payload = left.payload();
    assert!(payload.error.is_none());
    assert!(payload.reason_error.unwrap().contains("write leave reason"));
    assert_eq!(payload.before.as_deref(), Some("tuning"));
}

#[derive(Clone, Copy, Debug)]
enum HandoffFault {
    Lookup,
    Append,
    Reset,
    Clear,
}

/// A session on a later boot whose journal holds a dead end still to be finished, with `prior` pending in `grub`.
fn resumed_dead_end(grub: &Grub, prior: &LeaveReason) -> (Bench, crate::session::Session) {
    let bench = Bench::new(2);
    let mut first = crate::testing::begun(&bench);
    first
        .appender
        .append(
            Payload::DeadEnd(DeadEnd {
                condition: DeadEndCondition::BootLoop,
                core: None,
                detail: "interrupted handoff".into(),
                action: Some(DeadEndAction::ClearSavedEntryAndReboot),
            }),
            &[],
        )
        .unwrap();
    drop(first);
    bench.sim.reboot();
    grub.pending(prior);
    let mut input = bench.input();
    input.bootloader = Some(Box::new(grub.clone()));
    let Ok(session) = crate::session::Session::open(input) else {
        panic!("open");
    };
    (bench, session)
}

#[test]
fn a_resumed_dead_end_whose_pending_reason_cannot_be_acknowledged_stops_before_the_grub_handoff() {
    for fault in [
        HandoffFault::Lookup,
        HandoffFault::Append,
        HandoffFault::Reset,
        HandoffFault::Clear,
    ] {
        let grub = Grub::with_count("2");
        let prior = exhausted();
        let (bench, mut session) = resumed_dead_end(&grub, &prior);
        let before = session.appender().journal().events().len();
        match fault {
            HandoffFault::Lookup => bench.mem.inject(Fault::before(Op::Read, 1)),
            HandoffFault::Append => bench
                .mem
                .inject(Fault::before(Op::Write, 1).named("events.jsonl")),
            HandoffFault::Reset => grub.refuse(COUNT_VARIABLE),
            HandoffFault::Clear => grub.refuse(REASON_VARIABLE),
        }
        let error = session.resume_dead_end().unwrap_err();
        let kept = matches!(
            (fault, &error),
            (HandoffFault::Lookup, SessionError::Lookup(_))
                | (HandoffFault::Append, SessionError::Journal(_))
                | (
                    HandoffFault::Reset,
                    SessionError::Bootloader(togi_tuningboot::Error::ResetCount(_))
                )
                | (
                    HandoffFault::Clear,
                    SessionError::Bootloader(togi_tuningboot::Error::ClearReason(_))
                )
        );
        assert!(kept, "{fault:?}: {error:?}");
        assert_eq!(
            session.appender().failure().is_some(),
            matches!(fault, HandoffFault::Append),
            "{fault:?}: only a failed append latches the journal"
        );
        assert_eq!(grub.0.borrow().entries_cleared, 0, "{fault:?}");
        let pending = togi_tuningboot::read_reason(&mut grub.clone())
            .unwrap()
            .unwrap();
        assert_eq!(pending.id, prior.id, "{fault:?}: the pending reason was replaced");
        let added: Vec<_> = session.appender().journal().events()[before..]
            .iter()
            .map(|event| event.kind())
            .collect();
        assert!(
            added.iter().all(|kind| *kind == EventKind::BootLeaveReason),
            "{fault:?}: {added:?}"
        );
        assert!(
            !session.appender().journal().events().iter().any(|event| matches!(
                event.payload,
                Payload::BootSavedEntry(_) | Payload::Shutdown(_)
            )),
            "{fault:?}: a handoff event authorizes a reboot"
        );
    }
}
