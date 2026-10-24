use std::cell::{Cell, RefCell};
use std::io;
use std::num::NonZeroU32;
use std::rc::Rc;
use test_case::test_case;

use togi_journal::fs::{Fault, Op};
use togi_journal::{
    Attribution, DeadEnd, DeadEndAction, DeadEndCondition, Event, EventKind, Outcome, Payload,
    ShutdownReason,
};
use togi_machine::{
    CancellationToken, Error, Partial, Running, Signal, Smu, SmuValidated, Stopped, TargetOffset,
    TrialResult, TrialSpec, Trials,
};
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;
use togi_sim::faults::{HookPhase, SmuOperation};
use togi_sim::model::Script;
use togi_tuningboot::{Bootloader, Environment, SavedEntry};

use crate::testing::{Bench, begun, core, observed, put};
use crate::{Ended, SessionError, Stop, run};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Clear {
    Works,
    FailsUntouched,
    FailsAfterClearing,
}

struct Grub {
    saved: String,
    clears: u32,
    clear: Clear,
}

#[derive(Clone)]
struct SharedGrub(Rc<RefCell<Grub>>);

impl SharedGrub {
    fn new(clear: Clear) -> Self {
        Self(Rc::new(RefCell::new(Grub {
            saved: "togi".into(),
            clears: 0,
            clear,
        })))
    }

    fn healthy() -> Self {
        Self::new(Clear::Works)
    }

    fn clears(&self) -> u32 {
        self.0.borrow().clears
    }

    fn saved(&self) -> String {
        self.0.borrow().saved.clone()
    }

    fn repair(&self) {
        self.0.borrow_mut().clear = Clear::Works;
    }

    fn boxed(&self) -> Box<dyn Bootloader> {
        Box::new(self.clone())
    }
}

impl Environment for SharedGrub {
    fn get(&mut self, _: &str) -> io::Result<Option<String>> {
        Ok(None)
    }

    fn set(&mut self, _: &[(&str, &str)]) -> io::Result<()> {
        Ok(())
    }

    fn unset(&mut self, _: &[&str]) -> io::Result<()> {
        Ok(())
    }
}

impl Bootloader for SharedGrub {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        let mut grub = self.0.borrow_mut();
        grub.clears += 1;
        let before = grub.saved.clone();
        match grub.clear {
            Clear::Works => {
                grub.saved.clear();
                Ok(SavedEntry {
                    before,
                    after: String::new(),
                })
            }
            Clear::FailsUntouched => Err(io::Error::other("grub-editenv unset failed")),
            Clear::FailsAfterClearing => {
                grub.saved.clear();
                Err(io::Error::other("grub-editenv unset failed"))
            }
        }
    }
}

fn quiet() -> Bench {
    Bench::defined(&MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    })
}

fn input(bench: &Bench, grub: Option<&SharedGrub>) -> crate::RunInput<'static> {
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.bootloader = grub.map(SharedGrub::boxed);
    input
}

fn started_lower(bench: &Bench) -> crate::RunInput<'static> {
    let mut input = input(bench, None);
    input.config = togi_config::Config::parse(r#"{"start_offsets":{"0":-10,"1":-10}}"#).unwrap();
    input
}

fn drive_with(bench: &Bench, grub: Option<&SharedGrub>, cancelled: bool) -> Ended {
    for _ in 0..100 {
        if bench.sim.seams().host.boot_id().is_err() {
            bench.sim.reboot().unwrap();
        }
        let input = input(bench, grub);
        if cancelled {
            input.cancel.cancel();
        }
        let ended = run(input);
        if !ended.crashed() {
            return ended;
        }
        bench.sim.crash();
    }
    panic!("the machine kept crashing");
}

fn drive(bench: &Bench, grub: Option<&SharedGrub>) -> Ended {
    drive_with(bench, grub, false)
}

fn seq_of(events: &[Event], kind: EventKind) -> u64 {
    events
        .iter()
        .find(|event| event.kind() == kind)
        .unwrap_or_else(|| panic!("no {kind:?} event"))
        .seq
        .get()
}

fn count_after(events: &[Event], seq: u64, kind: EventKind) -> usize {
    events
        .iter()
        .filter(|event| event.seq.get() > seq && event.kind() == kind)
        .count()
}

fn saved_entries(events: &[Event]) -> Vec<&Event> {
    events
        .iter()
        .filter(|event| matches!(event.payload, Payload::BootSavedEntry(_)))
        .collect()
}

fn interrupt(bench: &Bench, fault: Fault) {
    bench.mem.inject(fault.named("events.jsonl"));
}

fn assert_interrupted(ended: &Ended) {
    assert!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, SessionError::Journal(_))),
        "{:?}",
        ended.errors
    );
}

fn corrupted_readback(bench: &Bench) {
    bench.sim.corrupt_readback(core(0));
}

fn boot_loop(bench: &Bench) {
    let mut crashes = 0;
    bench.sim.interrupt_smu(move |operation: &SmuOperation| {
        if operation.phase == HookPhase::Before && crashes < 3 {
            crashes += 1;
            return Err(Error::Crashed);
        }
        Ok(())
    });
}

#[test_case("clear", corrupted_readback, DeadEndAction::ClearSavedEntry, false, false; "clear")]
#[test_case("clear and reboot", boot_loop, DeadEndAction::ClearSavedEntryAndReboot, true, false; "clear and reboot")]
#[test_case("failed clear", boot_loop, DeadEndAction::ClearSavedEntryAndReboot, false, true; "failed clear")]
fn a_dead_end_interrupted_before_the_saved_entry_clear_resumes_and_clears_it_once(
    case_name: &str,
    case_arm: fn(&Bench),
    case_action: DeadEndAction,
    case_reboot: bool,
    case_fails_clear: bool,
) {
    let reference = quiet();
    (case_arm)(&reference);
    let healthy = SharedGrub::healthy();
    let finished = drive(&reference, Some(&healthy));
    let dead_end = seq_of(finished.journal.events(), EventKind::DeadEnd);
    drop(finished);

    let bench = quiet();
    (case_arm)(&bench);
    let grub = SharedGrub::new(if case_fails_clear {
        Clear::FailsUntouched
    } else {
        Clear::Works
    });
    interrupt(&bench, Fault::after(Op::Write, dead_end));
    let interrupted = drive(&bench, Some(&grub));
    assert_interrupted(&interrupted);
    assert_eq!(grub.clears(), 0, "{case_name}");
    drop(interrupted);

    let resumed = drive(&bench, Some(&grub));
    assert!(
        resumed.errors.is_empty(),
        "{case_name}: {:?}",
        resumed.errors
    );
    assert!(
        matches!(&resumed.stop, Some(Stop::DeadEnd { dead_end, reboot, .. })
                if dead_end.action == Some(case_action) && *reboot == case_reboot),
        "{case_name}: {:?}",
        resumed.stop
    );
    assert_eq!(grub.clears(), 1, "{case_name}");
    let events = resumed.journal.events();
    assert_eq!(count_after(events, dead_end, EventKind::DeadEnd), 0);
    assert_eq!(count_after(events, dead_end, EventKind::TrialIntent), 0);
    assert_eq!(count_after(events, dead_end, EventKind::BootSavedEntry), 1);
    assert_eq!(count_after(events, dead_end, EventKind::Shutdown), 1);
    assert_eq!(events.last().unwrap().kind(), EventKind::Shutdown);
    let [entry] = saved_entries(events)[..] else {
        panic!("{case_name}: not one saved entry");
    };
    assert_eq!(
        entry.cause.iter().map(|seq| seq.get()).collect::<Vec<_>>(),
        [dead_end]
    );
    let Payload::BootSavedEntry(recorded) = &entry.payload else {
        unreachable!()
    };
    assert_eq!(recorded.error.is_some(), case_fails_clear, "{case_name}");
    assert!(bench.sim.violations().is_empty());
}

#[test]
fn a_dead_end_resumed_after_its_saved_entry_was_recorded_only_shuts_down() {
    let reference = quiet();
    reference.sim.corrupt_readback(core(0));
    let finished = drive(&reference, Some(&SharedGrub::healthy()));
    let entry = seq_of(finished.journal.events(), EventKind::BootSavedEntry);
    drop(finished);

    let bench = quiet();
    bench.sim.corrupt_readback(core(0));
    let grub = SharedGrub::healthy();
    interrupt(&bench, Fault::after(Op::Write, entry));
    let interrupted = drive(&bench, Some(&grub));
    assert_interrupted(&interrupted);
    drop(interrupted);

    let resumed = drive(&bench, Some(&grub));
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert!(matches!(resumed.stop, Some(Stop::DeadEnd { .. })));
    assert_eq!(grub.clears(), 1);
    let events = resumed.journal.events();
    assert_eq!(events.last().unwrap().kind(), EventKind::Shutdown);
    for event in events.iter().filter(|event| event.seq.get() > entry) {
        assert!(
            matches!(
                event.kind(),
                EventKind::Shutdown
                    | EventKind::DayBootStart
                    | EventKind::DayCheckpoint
                    | EventKind::DayOffsetsChanged
                    | EventKind::DayCleanStop
                    | EventKind::DayAnswer
                    | EventKind::DayRecord
                    | EventKind::DayFault
                    | EventKind::DayUncleanEnd
                    | EventKind::StateRebuilt
                    | EventKind::TrialEnd
                    | EventKind::PreflightCheck
                    | EventKind::SmuReadback
                    | EventKind::SmuIntent
                    | EventKind::SmuWrite
                    | EventKind::ProfileRestored
            ),
            "{:?} repeats work after the saved entry was recorded",
            event.kind()
        );
    }
    assert_eq!(count_after(events, entry, EventKind::BootSavedEntry), 0);
    assert_eq!(count_after(events, entry, EventKind::Shutdown), 1);
}

#[test]
fn a_dead_end_resumed_outside_the_tuning_boot_only_shuts_down_and_the_next_run_tunes() {
    let reference = quiet();
    reference.sim.corrupt_readback(core(0));
    let finished = drive(&reference, Some(&SharedGrub::healthy()));
    let dead_end = seq_of(finished.journal.events(), EventKind::DeadEnd);
    drop(finished);

    let bench = quiet();
    bench.sim.corrupt_readback(core(0));
    let grub = SharedGrub::healthy();
    interrupt(&bench, Fault::after(Op::Write, dead_end));
    let interrupted = drive(&bench, Some(&grub));
    assert_interrupted(&interrupted);
    drop(interrupted);

    let resumed = drive(&bench, None);
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert!(
        matches!(&resumed.stop, Some(Stop::DeadEnd { dead_end, reboot: false, .. })
            if dead_end.action == Some(DeadEndAction::ClearSavedEntry)),
        "{:?}",
        resumed.stop
    );
    assert_eq!(grub.clears(), 0);
    assert_eq!(grub.saved(), "togi");
    let events = resumed.journal.events();
    assert_eq!(count_after(events, dead_end, EventKind::BootSavedEntry), 0);
    assert_eq!(count_after(events, dead_end, EventKind::TrialIntent), 0);
    assert_eq!(count_after(events, dead_end, EventKind::Shutdown), 1);
    assert!(matches!(&events.last().unwrap().payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::DeadEnd));
    drop(resumed);

    let next = drive(&bench, None);
    assert!(next.errors.is_empty(), "{:?}", next.errors);
    assert_eq!(next.stop, Some(Stop::Cycles));
    assert!(count_after(next.journal.events(), dead_end, EventKind::TrialIntent) > 0);
    assert!(bench.sim.violations().is_empty());
}

#[test]
fn a_fully_recorded_dead_end_does_not_stop_the_next_run_from_checking_preflight_again() {
    let bench = quiet();
    bench.sim.fail_check("root", "uid 1000");
    let grub = SharedGrub::healthy();
    let first = drive(&bench, Some(&grub));
    assert!(matches!(&first.stop, Some(Stop::DeadEnd { dead_end, .. })
        if dead_end.condition == DeadEndCondition::Preflight));
    let before = first.journal.events().len();
    drop(first);

    let second = drive(&bench, Some(&grub));
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    assert!(matches!(&second.stop, Some(Stop::DeadEnd { dead_end, .. })
        if dead_end.condition == DeadEndCondition::Preflight));
    assert_eq!(grub.clears(), 2);
    let later = &second.journal.events()[before..];
    assert!(
        later
            .iter()
            .any(|event| event.kind() == EventKind::PreflightCheck)
    );
    assert!(
        later
            .iter()
            .all(|event| !matches!(event.kind(), EventKind::SmuIntent | EventKind::TrialIntent))
    );
    assert!(bench.sim.violations().is_empty());
}

#[test]
fn a_crash_then_a_failing_preflight_records_the_crash_before_the_dead_end_and_never_twice() {
    let bench = quiet();
    bench
        .sim
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: core(0),
                reset: None,
                then_crash: true,
            },
        )
        .unwrap();
    let crashed = run(started_lower(&bench));
    assert!(crashed.crashed(), "{:?}", crashed.errors);
    drop(crashed);
    bench.sim.reboot().unwrap();
    bench.sim.fail_check("root", "uid 1000");

    let blocked = run(started_lower(&bench));
    assert!(blocked.errors.is_empty(), "{:?}", blocked.errors);
    assert!(matches!(&blocked.stop, Some(Stop::DeadEnd { dead_end, .. })
        if dead_end.condition == DeadEndCondition::Preflight));
    let events = blocked.journal.events();
    let crash = events
        .iter()
        .position(|event| matches!(event.payload, Payload::CrashDetected(_)))
        .unwrap();
    let ended = events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::TrialEnd(end)
                if end.trial.as_str() == "0001" && end.outcome == Outcome::Failure)
        })
        .unwrap();
    let dead_end = events
        .iter()
        .position(|event| event.kind() == EventKind::DeadEnd)
        .unwrap();
    assert!(crash < ended && ended < dead_end);
    drop(blocked);

    bench.sim.fail_check("root", "");
    let tuned = run(started_lower(&bench));
    assert!(tuned.errors.is_empty(), "{:?}", tuned.errors);
    assert_eq!(tuned.stop, Some(Stop::Cycles));
    let failures = tuned
        .journal
        .events()
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::Failure(failure)
            if failure.trial.as_ref().is_some_and(|trial| trial.as_str() == "0001"))
        })
        .count();
    let crashes = tuned
        .journal
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::CrashDetected(_)))
        .count();
    let attributed = tuned
        .journal
        .events()
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::Failure(failure)
            if failure.trial.as_ref().is_some_and(|trial| trial.as_str() == "0001")
                && failure.attribution == Attribution::Attributed
                && failure.core == Some(core(0)))
        })
        .count();
    let ends = tuned
        .journal
        .events()
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::TrialEnd(end) if end.trial.as_str() == "0001")
        })
        .count();
    assert_eq!((failures, attributed, crashes, ends), (1, 1, 1, 1));
    assert!(bench.sim.violations().is_empty());
}

struct CancelAfterStart {
    inner: Box<dyn Trials>,
    cancel: CancellationToken,
}

impl Trials for CancelAfterStart {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<togi_machine::Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let running = self.inner.start(cancel, spec, ready)?;
        self.cancel.cancel();
        Ok(running)
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        self.inner.samples(id)
    }
}

#[test]
fn a_signal_stop_is_a_clean_stop_so_the_next_boot_is_no_crash_and_tuning_resumes() {
    let bench = quiet();
    let mut first = input(&bench, None);
    first.machine.trials = Box::new(CancelAfterStart {
        inner: first.machine.trials,
        cancel: first.cancel.clone(),
    });
    let stopped = run(first);
    assert!(stopped.errors.is_empty(), "{:?}", stopped.errors);
    assert_eq!(stopped.stop, Some(Stop::Signal));
    let last = stopped.journal.events().last().unwrap().clone();
    assert!(
        matches!(&last.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal)
    );
    let stopped_events = stopped.journal.events().len();
    drop(stopped);

    bench.sim.reboot().unwrap();
    let resumed = run(input(&bench, None));
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert_eq!(resumed.stop, Some(Stop::Cycles));
    let later = &resumed.journal.events()[stopped_events..];
    assert!(later.iter().all(|event| !matches!(&event.payload,
        Payload::CrashDetected(detected) if detected.previous_boot == last.boot)));
    assert!(
        later
            .iter()
            .any(|event| event.kind() == EventKind::TrialIntent && event.boot != last.boot)
    );
    assert!(bench.sim.violations().is_empty());
}

const HANDOFF: &str = "interrupted GRUB handoff";

struct Handoff {
    bench: Bench,
    dead_end: u64,
    events: usize,
}

fn pending_handoff() -> Handoff {
    let bench = quiet();
    let mut session = begun(&bench);
    put(&mut session, &[-30, -29]);
    let dead_end = session
        .appender
        .append(
            Payload::DeadEnd(DeadEnd {
                condition: DeadEndCondition::BootLoop,
                core: None,
                detail: HANDOFF.into(),
                action: Some(DeadEndAction::ClearSavedEntryAndReboot),
            }),
            &[],
        )
        .unwrap();
    let events = session.appender().journal().events().len();
    drop(session);
    Handoff {
        bench,
        dead_end: dead_end.get(),
        events,
    }
}

fn offsets_of(bench: &Bench) -> Vec<i64> {
    let mut smu = bench.sim.seams().smu;
    (0..2).map(|n| smu.offset(core(n)).unwrap().0).collect()
}

fn decisions(events: &[Event]) -> togi_tuner::Projection {
    togi_tuner::replay(events).unwrap().project()
}

fn resume_cancelled(handoff: &Handoff, grub: &SharedGrub) -> Ended {
    let input = input(&handoff.bench, Some(grub));
    input.cancel.cancel();
    run(input)
}

fn assert_cleanup_order(events: &[Event], dead_end: u64) {
    let after = |kind: EventKind| {
        events
            .iter()
            .position(|event| event.seq.get() > dead_end && event.kind() == kind)
    };
    let preflight = after(EventKind::PreflightCheck).unwrap();
    let restored = after(EventKind::ProfileRestored).unwrap();
    let entry = after(EventKind::BootSavedEntry).unwrap();
    let shutdown = events
        .iter()
        .position(|event| {
            event.seq.get() > dead_end
                && matches!(&event.payload, Payload::Shutdown(shutdown)
                    if shutdown.reason == ShutdownReason::DeadEnd)
        })
        .unwrap();
    assert!(preflight < restored && restored < entry && entry < shutdown);
    assert_eq!(count_after(events, dead_end, EventKind::TrialIntent), 0);
}

#[test_case("the clear failed and the process died before recording it", Clear::FailsUntouched, true, false, "togi", 2, false; "the clear failed and the process died before recording it")]
#[test_case("the entry was cleared and the process died before recording it", Clear::Works, true, false, "", 2, false; "the entry was cleared and the process died before recording it")]
#[test_case("the entry was recorded and the process died", Clear::Works, true, true, "togi", 1, false; "the entry was recorded and the process died")]
#[test_case("the process died before the shutdown was recorded", Clear::Works, false, false, "togi", 1, false; "the process died before the shutdown was recorded")]
#[test_case("the process died after the shutdown was recorded", Clear::Works, false, true, "togi", 1, true; "the process died after the shutdown was recorded")]
fn a_tuning_boot_handoff_interrupted_at_any_cleanup_boundary_resumes_without_repeating_or_skipping_it(
    case_name: &str,
    case_clear: Clear,
    case_entry_not_shutdown: bool,
    case_after_write: bool,
    case_before: &str,
    case_clears: u32,
    case_completed: bool,
) {
    let reference = pending_handoff();
    let finished = resume_cancelled(&reference, &SharedGrub::healthy());
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let entry_seq = seq_of(finished.journal.events(), EventKind::BootSavedEntry);
    let shutdown_seq = finished
        .journal
        .events()
        .iter()
        .find(|event| {
            matches!(&event.payload, Payload::Shutdown(shutdown)
                if shutdown.reason == ShutdownReason::DeadEnd)
        })
        .unwrap()
        .seq
        .get();
    drop(finished);
    let expected_offsets = offsets_of(&reference.bench);

    let handoff = pending_handoff();
    let facts = decisions(&begun_events(&handoff));
    let grub = SharedGrub::new(case_clear);
    let target = if case_entry_not_shutdown {
        entry_seq
    } else {
        shutdown_seq
    };
    let nth = target - u64::try_from(handoff.events).unwrap();
    interrupt(
        &handoff.bench,
        if case_after_write {
            Fault::after(Op::Write, nth)
        } else {
            Fault::before(Op::Write, nth)
        },
    );
    let interrupted = resume_cancelled(&handoff, &grub);
    assert_interrupted(&interrupted);
    if case_entry_not_shutdown && !case_after_write {
        assert!(
            !matches!(&interrupted.stop, Some(Stop::DeadEnd { reboot: true, .. })),
            "{case_name}: authorized a reboot before the cleanup was durable"
        );
    }
    drop(interrupted);
    assert_eq!(
        grub.saved() == "togi",
        case_clear == Clear::FailsUntouched,
        "{case_name}"
    );

    grub.repair();
    let resumed = resume_cancelled(&handoff, &grub);
    assert!(
        resumed.errors.is_empty(),
        "{case_name}: {:?}",
        resumed.errors
    );
    if case_completed {
        assert_eq!(resumed.stop, Some(Stop::Signal), "{case_name}");
    } else {
        assert!(
            matches!(&resumed.stop, Some(Stop::DeadEnd { dead_end, reboot: true, .. })
                    if dead_end.detail == HANDOFF),
            "{case_name}: {:?}",
            resumed.stop
        );
    }
    assert_eq!(grub.saved(), "", "{case_name}");
    assert_eq!(grub.clears(), case_clears, "{case_name}");
    let events = resumed.journal.events();
    assert_cleanup_order(events, handoff.dead_end);
    let [entry] = saved_entries(events)[..] else {
        panic!("{case_name}: duplicate or missing saved entry");
    };
    assert_eq!(
        entry.cause.iter().map(|seq| seq.get()).collect::<Vec<_>>(),
        [handoff.dead_end]
    );
    let Payload::BootSavedEntry(recorded) = &entry.payload else {
        unreachable!()
    };
    assert_eq!(recorded.before.as_deref(), Some(case_before), "{case_name}");
    assert_eq!(recorded.after.as_deref(), Some(""));
    assert_eq!(recorded.error, None);
    let shutdowns = events
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::Shutdown(shutdown)
                if shutdown.reason == ShutdownReason::DeadEnd)
        })
        .count();
    assert_eq!(shutdowns, 1, "{case_name}");
    assert_eq!(decisions(events), facts, "{case_name}");
    drop(resumed);
    assert_eq!(offsets_of(&handoff.bench), expected_offsets, "{case_name}");
    assert!(handoff.bench.sim.violations().is_empty());
}

fn begun_events(handoff: &Handoff) -> Vec<Event> {
    let session = handoff.bench.open();
    session.appender().journal().events().to_vec()
}

#[test_case(Clear::FailsUntouched; "Clear::FailsUntouched")]
#[test_case(Clear::FailsAfterClearing; "Clear::FailsAfterClearing")]
fn a_failed_boot_cleanup_never_authorizes_a_reboot_when_the_handoff_resumes(clear: Clear) {
    let reference = pending_handoff();
    let finished = resume_cancelled(&reference, &SharedGrub::new(clear));
    assert!(
        matches!(&finished.stop, Some(Stop::DeadEnd { reboot: false, .. })),
        "{clear:?}: {:?}",
        finished.stop
    );
    let shutdown = finished
        .journal
        .events()
        .iter()
        .find(|event| {
            matches!(&event.payload, Payload::Shutdown(shutdown)
                    if shutdown.reason == ShutdownReason::DeadEnd)
        })
        .unwrap()
        .seq
        .get();
    drop(finished);

    let handoff = pending_handoff();
    let facts = decisions(&begun_events(&handoff));
    let grub = SharedGrub::new(clear);
    interrupt(
        &handoff.bench,
        Fault::before(Op::Write, shutdown - u64::try_from(handoff.events).unwrap()),
    );
    let interrupted = resume_cancelled(&handoff, &grub);
    assert_interrupted(&interrupted);
    assert!(
        !matches!(&interrupted.stop, Some(Stop::DeadEnd { reboot: true, .. })),
        "{clear:?}"
    );
    drop(interrupted);

    let resumed = resume_cancelled(&handoff, &grub);
    assert!(resumed.errors.is_empty(), "{clear:?}: {:?}", resumed.errors);
    assert!(
        matches!(&resumed.stop, Some(Stop::DeadEnd { reboot: false, .. })),
        "{clear:?}: {:?}",
        resumed.stop
    );
    assert_eq!(grub.clears(), 1, "{clear:?}: the failed clear was repeated");
    assert_eq!(
        grub.saved().is_empty(),
        clear == Clear::FailsAfterClearing,
        "{clear:?}"
    );
    let events = resumed.journal.events();
    assert_cleanup_order(events, handoff.dead_end);
    let [entry] = saved_entries(events)[..] else {
        panic!("{clear:?}: duplicate or missing saved entry");
    };
    let Payload::BootSavedEntry(recorded) = &entry.payload else {
        unreachable!()
    };
    assert!(
        recorded
            .error
            .as_deref()
            .is_some_and(|error| error.contains("grub-editenv unset failed")),
        "{clear:?}: {recorded:?}"
    );
    assert_eq!(decisions(events), facts, "{clear:?}");
}

#[derive(Default)]
struct Joins {
    active: Cell<bool>,
    started: Cell<u32>,
    joined: Cell<u32>,
    finalized: RefCell<Vec<String>>,
}

struct OwnedTrials {
    inner: Box<dyn Trials>,
    joins: Rc<Joins>,
    cancel_when_finalized: Option<CancellationToken>,
}

struct OwnedRunning {
    inner: Box<dyn Running>,
    joins: Rc<Joins>,
}

struct OwnedSmu {
    inner: Box<dyn Smu>,
    joins: Rc<Joins>,
}

impl Trials for OwnedTrials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<togi_machine::Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let running = self.inner.start(cancel, spec, ready)?;
        self.joins.active.set(true);
        self.joins.started.set(self.joins.started.get() + 1);
        Ok(Box::new(OwnedRunning {
            inner: running,
            joins: Rc::clone(&self.joins),
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        assert!(
            !self.joins.active.get(),
            "trial {id} files finalized before its workload joined"
        );
        self.joins.finalized.borrow_mut().push(id.to_owned());
        self.inner.ended(id)?;
        if let Some(cancel) = &self.cancel_when_finalized {
            cancel.cancel();
        }
        Ok(())
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        self.inner.samples(id)
    }
}

impl Running for OwnedRunning {
    fn started(&self) -> togi_machine::Started {
        self.inner.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn togi_machine::Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        let result = self.inner.wait(cancel, report);
        self.joins.active.set(false);
        self.joins.joined.set(self.joins.joined.get() + 1);
        result
    }

    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        let Self { inner, joins } = *self;
        let stopped = inner.stop();
        joins.active.set(false);
        joins.joined.set(joins.joined.get() + 1);
        stopped
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the wrapper forwards the session's own writes, checking that no workload runs"
)]
impl Smu for OwnedSmu {
    fn offset(
        &mut self,
        core: togi_machine::CoreId,
    ) -> Result<togi_machine::ObservedOffset, Error> {
        self.inner.offset(core)
    }

    fn set_offset(
        &mut self,
        core: togi_machine::CoreId,
        offset: TargetOffset,
    ) -> Result<(), Error> {
        assert!(
            !self.joins.active.get(),
            "offset written before the workload joined"
        );
        self.inner.set_offset(core, offset)
    }

    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
        assert!(
            !self.joins.active.get(),
            "offsets written before the workload joined"
        );
        self.inner.set_all_offsets(offset)
    }

    fn emergency_zero(&mut self, validated: SmuValidated, stopped: &Stopped) -> Result<(), Error> {
        assert!(
            !self.joins.active.get(),
            "emergency zero before the workload joined"
        );
        self.inner.emergency_zero(validated, stopped)
    }
}

fn owned_input(
    bench: &Bench,
    joins: &Rc<Joins>,
    cancel_when_finalized: bool,
) -> crate::RunInput<'static> {
    let mut input = bench.input();
    input.config = togi_config::Config::parse(r#"{"start_offsets":{"0":-5,"1":-5}}"#).unwrap();
    input.machine.trials = Box::new(OwnedTrials {
        inner: input.machine.trials,
        joins: Rc::clone(joins),
        cancel_when_finalized: cancel_when_finalized.then(|| input.cancel.clone()),
    });
    input.machine.smu = Box::new(OwnedSmu {
        inner: input.machine.smu,
        joins: Rc::clone(joins),
    });
    input
}

fn assert_owned_exit(bench: &Bench, joins: &Joins, ended: &Ended) {
    assert!(joins.started.get() >= 1);
    assert!(
        !joins.active.get(),
        "the journal left a running workload behind"
    );
    assert!(joins.joined.get() >= joins.started.get());
    assert!(
        bench.sim.violations().is_empty(),
        "{:?}",
        bench.sim.violations()
    );
    assert!(!ended.journal.events().is_empty());
}

fn assert_restored_and_published(bench: &Bench, ended: &Ended) {
    let baseline = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::SessionBaseline(baseline) = &event.payload {
                Some(baseline.offsets)
            } else {
                None
            }
        })
        .unwrap();
    let held = offsets_of(bench);
    assert_eq!(observed(&held), baseline);
    let facts = togi_journal::State::replay(ended.journal.events()).unwrap();
    let tuner = togi_tuner::replay(ended.journal.events()).unwrap();
    let (_, state): (_, crate::publication::StateDocument) =
        ended.journal.dir().read_snapshot().unwrap().unwrap();
    assert_eq!(state.facts, facts);
    assert_eq!(state.tuner, tuner.project());
}

#[test_case("signal after a passed trial", &Stop::Signal; "signal after a passed trial")]
#[test_case("requested cycles", &Stop::Cycles; "requested cycles")]
#[test_case("first confirmed result", &Stop::FirstResult; "first confirmed result")]
fn every_stop_and_error_exit_joins_the_trial_restores_the_hardware_publishes_the_state_and_returns_the_journal(
    case_name: &str,
    case_stop: &Stop,
) {
    let bench = quiet();
    let joins = Rc::new(Joins::default());
    let mut input = owned_input(&bench, &joins, *case_stop == Stop::Signal);
    match case_stop {
        Stop::Cycles => input.policy.cycles = Some(NonZeroU32::MIN),
        Stop::FirstResult => input.policy.first_result = true,
        Stop::Signal | Stop::DeadEnd { .. } => {}
    }
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{case_name}: {:?}", ended.errors);
    assert_eq!(ended.stop, Some(case_stop.clone()), "{case_name}");
    assert!(ended.emergency.is_none(), "{case_name}");
    assert_owned_exit(&bench, &joins, &ended);
    assert_restored_and_published(&bench, &ended);
    assert!(
        matches!(
            &ended.journal.events().last().unwrap().payload,
            Payload::Shutdown(_)
        ),
        "{case_name}"
    );

    let bench = quiet();
    bench
        .sim
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    let joins = Rc::new(Joins::default());
    let mut input = owned_input(&bench, &joins, false);
    input.config = togi_config::Config::parse(r#"{"start_offsets":{"0":0,"1":0}}"#).unwrap();
    input.policy.cycles = Some(NonZeroU32::MIN);
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert!(matches!(&ended.stop, Some(Stop::DeadEnd { dead_end, .. })
        if dead_end.condition == DeadEndCondition::FailureAtZero));
    assert_owned_exit(&bench, &joins, &ended);
    assert_restored_and_published(&bench, &ended);

    let reference = quiet();
    let joins = Rc::new(Joins::default());
    let finished = run(owned_input(&reference, &joins, true));
    let trial_start = seq_of(finished.journal.events(), EventKind::TrialStart);
    drop(finished);

    let bench = quiet();
    let joins = Rc::new(Joins::default());
    interrupt(&bench, Fault::before(Op::Write, trial_start));
    let ended = run(owned_input(&bench, &joins, true));
    assert!(
        matches!(ended.errors.first(), Some(SessionError::Journal(_))),
        "{:?}",
        ended.errors
    );
    assert!(ended.stop.is_none());
    assert!(ended.emergency.is_some());
    assert_owned_exit(&bench, &joins, &ended);
    assert_eq!(offsets_of(&bench), [0, 0]);
}
