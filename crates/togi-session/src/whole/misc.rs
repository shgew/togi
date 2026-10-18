use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Duration;
use test_case::test_case;

use togi_config::Config;
use togi_defect::{Clause, Entry};
use togi_journal::{
    CorePhase, Decision, DefectDirection, DefectId, Event, EventKind, FixStamp, Payload,
    RetryTarget, Seq, ShutdownReason,
};
use togi_machine::{
    BootId, CancellationToken, Clock, CoreId, Error, Kernel, KernelCursor, KernelRead,
    ObservedOffset, Partial, PstoreRecord, Regime, ResetKind, ResetReason, Running, Signal, Smu,
    SmuValidated, Stopped, Swept, TargetOffset, TrialSpec, Trials, workloads,
};
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;
use togi_sim::model::Script;

use crate::session::RunInput;
use crate::testing::{Bench, MemBoot, core, target};
use crate::{Ended, Stop, run};

fn bench() -> Bench {
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

fn one_cycle(bench: &Bench, adjust: impl FnOnce(&mut RunInput<'static>)) -> Ended {
    let mut input = bench.input();
    input.config = power_off_config();
    input.policy.cycles = Some(NonZeroU32::MIN);
    adjust(&mut input);
    run(input)
}

fn assert_no_violations(bench: &Bench) {
    assert!(
        bench.sim.violations().is_empty(),
        "{:?}",
        bench.sim.violations()
    );
}

fn script(bench: &Bench, trial: &str, signal: Signal, reset: Option<ResetKind>, crash: bool) {
    bench
        .sim
        .script_trial(
            trial,
            Script {
                signal: Some(signal),
                at_s: 2.0,
                core: core(0),
                reset,
                then_crash: crash,
            },
        )
        .unwrap();
}

#[test]
fn configured_candidate_limits_start_checking_with_frozen_r1_r2_workloads() {
    let bench = bench();
    let ended = one_cycle(&bench, |input| {
        input.config = Config::parse(r#"{"candidate_solo_limits":{"0":-12,"1":-13}}"#).unwrap();
    });
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    assert_no_violations(&bench);
    let events = ended.journal.events();
    let frozen = [workloads(Regime::R1)[0].id, workloads(Regime::R2)[0].id];
    let initial: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::CorePhase(phase) = &event.payload
                && phase.from.is_none()
            {
                Some((
                    phase.core,
                    phase.to,
                    phase.offset,
                    phase.check_solo_limit,
                    phase.reason.as_str(),
                    phase
                        .workloads
                        .iter()
                        .map(|workload| workload.as_str())
                        .collect::<Vec<_>>(),
                ))
            } else {
                None
            }
        })
        .collect();
    let expected = |n: i64, offset: i64| {
        (
            core(n),
            CorePhase::Search,
            target(offset),
            true,
            "configured candidate solo limit",
            frozen.to_vec(),
        )
    };
    assert_eq!(initial, [expected(0, -12), expected(1, -13)]);
    for checked in [core(0), core(1)] {
        for (regime, workload) in [Regime::R1, Regime::R2].into_iter().zip(frozen) {
            let first = events
                .iter()
                .find_map(|event| {
                    if let Payload::TrialIntent(intent) = &event.payload
                        && intent.core == Some(checked)
                        && intent.regime == regime
                    {
                        Some(intent)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| panic!("no {regime:?} trial of core {checked:?}"));
            assert_eq!(first.workload.as_str(), workload);
            assert_eq!(
                first.offset,
                Some(target(if checked == core(0) { -12 } else { -13 }))
            );
        }
    }
}

fn boots_of(events: &[Event]) -> BTreeSet<BootId> {
    events.iter().map(|event| event.boot.clone()).collect()
}

fn last_cycle(events: &[Event]) -> Option<NonZeroU32> {
    events
        .iter()
        .filter_map(|event| {
            if let Payload::CheckingCycle(cycle) = &event.payload {
                Some(cycle.cycle)
            } else {
                None
            }
        })
        .max()
}

#[test]
fn a_resumed_checking_session_continues_its_numbering_on_new_boots_in_time_order() {
    let bench = bench();
    script(&bench, "0001", Signal::Crash, None, true);
    let crashed = one_cycle(&bench, |_| {});
    assert!(crashed.crashed(), "{:?}", crashed.errors);
    drop(crashed);
    bench.sim.reboot();
    let first = one_cycle(&bench, |_| {});
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    assert_eq!(first.stop, Some(Stop::Cycles));
    let earlier = first.journal.events().to_vec();
    drop(first);
    let boots = boots_of(&earlier);
    assert_eq!(boots.len(), 2);
    let cycle = last_cycle(&earlier).unwrap();

    bench.sim.reboot();
    let resumed = one_cycle(&bench, |input| {
        input.policy.cycles = NonZeroU32::new(2);
    });
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert_eq!(resumed.stop, Some(Stop::Cycles));
    assert_no_violations(&bench);
    let events = resumed.journal.events();
    assert_eq!(&events[..earlier.len()], earlier.as_slice());
    let appended = &events[earlier.len()..];
    assert!(!appended.is_empty());
    let mut last = earlier.last().unwrap();
    for event in appended {
        assert_eq!(event.seq, last.seq.next().unwrap(), "{event:?}");
        assert!(
            !boots.contains(&event.boot),
            "seq {:?} reuses boot {:?}",
            event.seq,
            event.boot
        );
        assert!(
            event.time >= last.time,
            "seq {:?} goes back in time",
            event.seq
        );
        last = event;
    }
    assert_eq!(boots_of(appended).len(), 1);
    assert!(last_cycle(appended).unwrap() > cycle);
}

const BACKOFF_ON_FAILURE: Clause = Clause {
    decision_kind: EventKind::TunerDecision,
    decision: Decision::Backoff,
    cause_kind: EventKind::Failure,
};

static POWER_OFF: [Entry; 1] = [Entry {
    id: DefectId::new(1).unwrap(),
    title: "power-off false failure",
    detail: "an unexpected backend exit was taken for a failure",
    pr: 100,
    direction: DefectDirection::TooCautious,
    clauses: &[BACKOFF_ON_FAILURE],
}];

fn power_off_config() -> Config {
    Config::parse(r#"{"start_offsets":{"0":-10,"1":-10}}"#).unwrap()
}

fn findings(events: &[Event]) -> Vec<&Event> {
    events
        .iter()
        .filter(|event| matches!(event.payload, Payload::DefectFound(_)))
        .collect()
}

#[test]
fn a_defect_in_decisions_before_its_fix_is_found_once_however_often_the_session_resumes() {
    let mut bench = bench();
    script(&bench, "0001", Signal::UnexpectedExit, None, false);
    let historic = one_cycle(&bench, |_| {});
    assert!(historic.errors.is_empty(), "{:?}", historic.errors);
    let failures: BTreeSet<Seq> = historic
        .journal
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::Failure(_)))
        .map(|event| event.seq)
        .collect();
    let decisions: Vec<Seq> = historic
        .journal
        .events()
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::TunerDecision(decision)
                if decision.decision == Decision::Backoff)
                && event.cause.iter().any(|cause| failures.contains(cause))
        })
        .map(|event| event.seq)
        .collect();
    assert!(!decisions.is_empty());
    assert!(findings(historic.journal.events()).is_empty());
    drop(historic);

    bench.defects = &POWER_OFF;
    let fixed = |input: &mut RunInput<'static>| {
        input.build.fixes = FixStamp::through(POWER_OFF[0].id);
    };
    for resume in 0..2 {
        let ended = one_cycle(&bench, fixed);
        assert!(
            ended.errors.is_empty(),
            "resume {resume}: {:?}",
            ended.errors
        );
        assert_eq!(ended.stop, Some(Stop::Cycles), "resume {resume}");
        let found = findings(ended.journal.events());
        assert_eq!(found.len(), 1, "resume {resume}");
        let Payload::DefectFound(finding) = &found[0].payload else {
            unreachable!()
        };
        assert_eq!(finding.id, POWER_OFF[0].id);
        assert_eq!(finding.cores.iter().collect::<Vec<_>>(), [core(0)]);
        assert_eq!(finding.decisions, decisions);
        assert_eq!(found[0].cause, finding.decisions);
    }
    assert_no_violations(&bench);
}

struct UnreadableOnce {
    inner: Box<dyn Kernel>,
    boot: BootId,
    failed: Rc<Cell<bool>>,
}

impl UnreadableOnce {
    fn fails(&self, boot: &BootId) -> bool {
        *boot == self.boot && !self.failed.replace(true)
    }
}

fn unreadable() -> Error {
    Error::Other("simulated unreadable kernel log".into())
}

impl Kernel for UnreadableOnce {
    fn mces(
        &mut self,
        boot: &BootId,
        since: Duration,
    ) -> Result<Vec<togi_machine::Mce>, Partial<Vec<togi_machine::Mce>>> {
        if self.fails(boot) {
            return Err(Partial {
                value: Vec::new(),
                error: unreadable(),
            });
        }
        self.inner.mces(boot, since)
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<togi_machine::Mce>>> {
        if self.fails(boot) {
            return Err(Partial {
                value: Vec::new(),
                error: unreadable(),
            });
        }
        self.inner.read_mces(boot, after)
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        if self.fails(boot) {
            return Err(unreadable());
        }
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        if self.fails(boot) {
            return Err(unreadable());
        }
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, Error> {
        self.inner.saved_pstore(boot)
    }
}

struct SignalledWhileWaiting {
    inner: Box<dyn Clock>,
    cancel: CancellationToken,
    waits: Rc<Cell<u32>>,
}

impl Clock for SignalledWhileWaiting {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> Duration {
        self.inner.monotonic()
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        self.waits.set(self.waits.get() + 1);
        self.cancel.cancel();
        self.inner.sleep(cancel, duration)
    }
}

fn ends_of(events: &[Event], trial: &str) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(&event.payload, Payload::TrialEnd(end) if end.trial.as_str() == trial),
        )
        .count()
}

#[test]
fn a_signal_during_the_kernel_log_retry_wait_stops_the_run_and_leaves_the_crashed_trial_open() {
    let bench = bench();
    script(
        &bench,
        "0001",
        Signal::Crash,
        Some(ResetKind::Watchdog),
        false,
    );
    let crashed = one_cycle(&bench, |_| {});
    assert!(crashed.crashed(), "{:?}", crashed.errors);
    let crashed_boot = crashed.journal.events().last().unwrap().boot.clone();
    drop(crashed);
    bench.sim.reboot();

    let failed = Rc::new(Cell::new(false));
    let waits = Rc::new(Cell::new(0));
    let interrupted = one_cycle(&bench, |input| {
        input.machine.kernel = Box::new(UnreadableOnce {
            inner: std::mem::replace(&mut input.machine.kernel, bench.sim.seams().kernel),
            boot: crashed_boot.clone(),
            failed: failed.clone(),
        });
        input.machine.clock = Box::new(SignalledWhileWaiting {
            inner: std::mem::replace(&mut input.machine.clock, bench.sim.seams().clock),
            cancel: input.cancel.clone(),
            waits: waits.clone(),
        });
    });
    assert!(interrupted.errors.is_empty(), "{:?}", interrupted.errors);
    assert_eq!(interrupted.stop, Some(Stop::Signal));
    assert!(failed.get());
    assert_eq!(waits.get(), 1);
    let events = interrupted.journal.events();
    assert!(
        matches!(&events.last().unwrap().payload, Payload::Shutdown(shutdown)
            if shutdown.reason == ShutdownReason::Signal)
    );
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::BackendRetry(retry) if retry.backend == RetryTarget::KernelLog)));
    assert_eq!(ends_of(events, "0001"), 0);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, Payload::CrashDetected(_) | Payload::TrialIntent(_) if event.boot != crashed_boot))
    );
    drop(interrupted);

    let resumed = one_cycle(&bench, |_| {});
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert_eq!(resumed.stop, Some(Stop::Cycles));
    assert_no_violations(&bench);
    let events = resumed.journal.events();
    let detected: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::CrashDetected(detected) = &event.payload {
                Some(detected)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(detected.len(), 1, "{detected:?}");
    assert!(!detected[0].stray);
    assert_eq!(detected[0].previous_boot, crashed_boot);
    assert_eq!(ends_of(events, "0001"), 1);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Touch {
    Sweep,
    Start,
    Write,
}

type Timeline = Rc<RefCell<Vec<Touch>>>;

/// Records every write that reaches the SMU, in the order it happens beside the trial seam's calls.
struct WatchedSmu {
    inner: Box<dyn Smu>,
    timeline: Timeline,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the wrapper forwards every SMU write so the test can observe it"
)]
impl Smu for WatchedSmu {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error> {
        self.inner.offset(core)
    }

    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error> {
        self.timeline.borrow_mut().push(Touch::Write);
        self.inner.set_offset(core, offset)
    }

    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
        self.timeline.borrow_mut().push(Touch::Write);
        self.inner.set_all_offsets(offset)
    }

    fn emergency_zero(&mut self, validated: SmuValidated, stopped: &Stopped) -> Result<(), Error> {
        self.timeline.borrow_mut().push(Touch::Write);
        self.inner.emergency_zero(validated, stopped)
    }
}

struct WatchedTrials {
    inner: Box<dyn Trials>,
    timeline: Timeline,
    cancel: CancellationToken,
    sweep_fails: bool,
}

impl Trials for WatchedTrials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.timeline.borrow_mut().push(Touch::Sweep);
        if self.sweep_fails {
            return Err(Error::Containment(
                "stale scope cleanup cannot be confirmed".into(),
            ));
        }
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        stopped: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        self.timeline.borrow_mut().push(Touch::Start);
        let running = self.inner.start(cancel, spec, stopped);
        self.cancel.cancel();
        running
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

fn watched_run(bench: &Bench, tuning_boot: bool, sweep_fails: bool) -> (Ended, Vec<Touch>) {
    let timeline = Timeline::default();
    let mut input = bench.input();
    if tuning_boot {
        input.bootloader = Some(Box::new(MemBoot::default()));
    }
    input.machine.smu = Box::new(WatchedSmu {
        inner: std::mem::replace(&mut input.machine.smu, bench.sim.seams().smu),
        timeline: timeline.clone(),
    });
    input.machine.trials = Box::new(WatchedTrials {
        inner: std::mem::replace(&mut input.machine.trials, bench.sim.seams().trials),
        timeline: timeline.clone(),
        cancel: input.cancel.clone(),
        sweep_fails,
    });
    let ended = run(input);
    let touched = timeline.borrow().clone();
    (ended, touched)
}

#[derive(Clone, Copy, Debug)]
enum Startup {
    Fresh,
    SameBoot,
    Reboot,
}

fn is_smu_record(event: &Event) -> bool {
    matches!(event.payload, Payload::SmuIntent(_) | Payload::SmuWrite(_))
}

fn is_smu_access(event: &Event) -> bool {
    is_smu_record(event) || matches!(event.payload, Payload::SmuReadback(_))
}

fn assert_preflight_then_sweep(events: &[Event], expected_checks: &BTreeSet<String>) -> usize {
    let sweep = events
        .iter()
        .position(|event| matches!(&event.payload, Payload::PreflightCheck(check) if check.check == "trial_scopes"))
        .unwrap();
    let before: BTreeSet<String> = events[..sweep]
        .iter()
        .filter_map(|event| {
            if let Payload::PreflightCheck(check) = &event.payload {
                assert!(check.ok, "{check:?}");
                Some(check.check.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(
        expected_checks.is_subset(&before),
        "{expected_checks:?} not all before the sweep: {before:?}"
    );
    assert!(!events[..sweep].iter().any(is_smu_access));
    sweep
}

#[test_case(false, Startup::Fresh, false; "tuning boot false Fresh sweep fails false")]
#[test_case(false, Startup::Fresh, true; "tuning boot false Fresh sweep fails true")]
#[test_case(false, Startup::SameBoot, false; "tuning boot false SameBoot sweep fails false")]
#[test_case(false, Startup::SameBoot, true; "tuning boot false SameBoot sweep fails true")]
#[test_case(false, Startup::Reboot, false; "tuning boot false Reboot sweep fails false")]
#[test_case(false, Startup::Reboot, true; "tuning boot false Reboot sweep fails true")]
#[test_case(true, Startup::Fresh, false; "tuning boot true Fresh sweep fails false")]
#[test_case(true, Startup::Fresh, true; "tuning boot true Fresh sweep fails true")]
#[test_case(true, Startup::SameBoot, false; "tuning boot true SameBoot sweep fails false")]
#[test_case(true, Startup::SameBoot, true; "tuning boot true SameBoot sweep fails true")]
#[test_case(true, Startup::Reboot, false; "tuning boot true Reboot sweep fails false")]
#[test_case(true, Startup::Reboot, true; "tuning boot true Reboot sweep fails true")]
fn every_startup_runs_preflight_then_the_sweep_before_its_first_smu_write(
    tuning_boot: bool,
    startup: Startup,
    sweep_fails: bool,
) {
    let case = format!("tuning boot {tuning_boot}, {startup:?}, sweep fails {sweep_fails}");
    let bench = bench();
    let mut start = 0;
    if !matches!(startup, Startup::Fresh) {
        let (setup, _) = watched_run(&bench, tuning_boot, false);
        assert_eq!(setup.stop, Some(Stop::Signal), "{case}: {:?}", setup.errors);
        start = setup.journal.events().len();
        drop(setup);
        if matches!(startup, Startup::Reboot) {
            bench.sim.reboot();
        }
    }
    let expected_checks: BTreeSet<String> = bench
        .sim
        .seams()
        .host
        .preflight()
        .into_iter()
        .map(|check| check.name.clone())
        .collect();
    assert!(!expected_checks.is_empty());
    let (ended, touched) = watched_run(&bench, tuning_boot, sweep_fails);
    assert!(ended.errors.is_empty(), "{case}: {:?}", ended.errors);
    assert_no_violations(&bench);
    let events = &ended.journal.events()[start..];
    let sweep = assert_preflight_then_sweep(events, &expected_checks);
    let Payload::PreflightCheck(check) = &events[sweep].payload else {
        unreachable!()
    };
    if sweep_fails {
        assert!(!check.ok, "{case}");
        assert!(
            matches!(&ended.stop, Some(Stop::DeadEnd { dead_end, causes, .. })
                            if dead_end.condition == togi_journal::DeadEndCondition::Containment
                                && *causes == [events[sweep].seq]),
            "{case}: {:?}",
            ended.stop
        );
        assert_eq!(touched, [Touch::Sweep], "{case}");
        assert!(
            !events
                .iter()
                .any(|event| is_smu_record(event)
                    || matches!(event.payload, Payload::TrialIntent(_))),
            "{case}"
        );
    } else {
        assert!(check.ok, "{case}");
        assert_eq!(ended.stop, Some(Stop::Signal), "{case}");
        let at = |wanted: Touch| touched.iter().position(|touch| *touch == wanted);
        assert_eq!(touched.first(), Some(&Touch::Sweep), "{case}: {touched:?}");
        assert_eq!(
            touched
                .iter()
                .filter(|touch| **touch == Touch::Sweep)
                .count(),
            1,
            "{case}: {touched:?}"
        );
        assert_eq!(
            touched
                .iter()
                .filter(|touch| **touch == Touch::Start)
                .count(),
            1,
            "{case}: {touched:?}"
        );
        assert!(
            at(Touch::Write).is_some_and(|write| write < at(Touch::Start).unwrap()),
            "no SMU write before the start: {case}: {touched:?}"
        );
        let first_write = events.iter().position(is_smu_record).unwrap();
        let start = events
            .iter()
            .position(|event| matches!(event.payload, Payload::TrialStart(_)))
            .unwrap();
        assert!(sweep < first_write && first_write < start, "{case}");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.payload, Payload::TrialIntent(_)))
                .count(),
            1,
            "{case}"
        );
    }
}
