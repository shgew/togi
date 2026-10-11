use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::rc::Rc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{
    Activity, BackendRetry, CorePhase, CoreTransition, CrashDetected, DeadEndCondition, Event,
    Failure, Mce, Outcome, Payload, RetryTarget, Seq, TrialEnd, TrialId, TrialIntent,
    TrialProgress, fs::Fault, fs::MemFs, fs::Op,
};
use togi_machine::{
    BankType, BootId, CancellationToken, Clock, Condition, CoreSet, CpuId, Error, Kernel,
    KernelCursor, KernelRead, Partial, PerCore, PmTable, PstoreRecord, ReadSamples, Regime,
    ResetKind, ResetReason, Running, Signal, Stopped, Swept, TargetOffset, TrialConditions,
    TrialSpec, Trials, pick_workload,
};
use togi_render::Renderer;

use super::{
    KernelUnreadable, Recovered, Recovery, append_trial_end, prior_boots, same_boot_trial,
};
use crate::appender::Appender;
use crate::error::{Flow, SessionError, Step};
use crate::recovery::tests::{Log, boot, dead_end, hunt_group};
use crate::testing::{Rig, assert_fold_consistent, core, simulated, target};

#[derive(Default)]
struct ClockState {
    sleeps: Vec<Duration>,
    cancel_at: Option<usize>,
    crash_at: Option<usize>,
}

struct TestClock(Rc<RefCell<ClockState>>);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap()
    }

    fn monotonic(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut state = self.0.borrow_mut();
        let at = state.sleeps.len();
        state.sleeps.push(duration);
        if state.cancel_at == Some(at) {
            cancel.cancel();
            return Err(Error::Cancelled);
        }
        if state.crash_at == Some(at) {
            return Err(Error::Crashed);
        }
        Ok(())
    }
}

enum Pstore {
    Record(PstoreRecord),
    Fails,
}

#[derive(Default)]
struct KernelScript {
    mces: BTreeMap<BootId, Vec<togi_machine::Mce>>,
    reasons: BTreeMap<BootId, ResetReason>,
    order: Vec<BootId>,
    gone: BTreeSet<BootId>,
    pstore: BTreeMap<BootId, Pstore>,
    failing: Option<BootId>,
    failures: u32,
    crashed: bool,
    reads: Vec<BootId>,
}

struct TestKernel(Rc<RefCell<KernelScript>>);

impl Kernel for TestKernel {
    fn mces(&mut self, boot: &BootId, since: Duration) -> Result<Vec<togi_machine::Mce>, Error> {
        let mut script = self.0.borrow_mut();
        if script.crashed {
            return Err(Error::Crashed);
        }
        script.reads.push(boot.clone());
        if script.gone.contains(boot) {
            return Err(Error::BootMissing);
        }
        if script.failing.as_ref() == Some(boot) && script.failures > 0 {
            script.failures -= 1;
            return Err(Error::Other("kernel log unreadable".into()));
        }
        Ok(script
            .mces
            .get(boot)
            .into_iter()
            .flatten()
            .filter(|mce| mce.monotonic >= since)
            .cloned()
            .collect())
    }

    fn read_mces(
        &mut self,
        _boot: &BootId,
        _after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<togi_machine::Mce>>> {
        Ok(KernelRead::default())
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        let script = self.0.borrow();
        if script.crashed {
            return Err(Error::Crashed);
        }
        if script.gone.contains(boot) {
            return Err(Error::BootMissing);
        }
        script.reasons.get(boot).cloned().ok_or(Error::BootMissing)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        let script = self.0.borrow();
        if script.crashed {
            return Err(Error::Crashed);
        }
        let next = script
            .order
            .iter()
            .position(|known| known == boot)
            .and_then(|at| script.order.get(at + 1))
            .ok_or(Error::BootMissing)?;
        script.reasons.get(next).cloned().ok_or(Error::BootMissing)
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, Error> {
        match self.0.borrow().pstore.get(boot) {
            None => Ok(None),
            Some(Pstore::Record(record)) => Ok(Some(record.clone())),
            Some(Pstore::Fails) => Err(Error::Other("pstore unreadable".into())),
        }
    }
}

#[derive(Clone, Copy)]
enum Compression {
    Works,
    Crashes,
    Fails,
}

struct TrialsScript {
    jsonl: BTreeMap<String, String>,
    conditions: BTreeMap<String, Vec<TrialConditions>>,
    ended: Vec<String>,
    compression: Compression,
}

struct TestTrials(Rc<RefCell<TrialsScript>>);

impl Trials for TestTrials {
    fn sweep(&mut self, _cancel: &CancellationToken) -> Result<Swept, Error> {
        Err(Error::Other("no sweep in recovery".into()))
    }

    fn start(
        &mut self,
        _cancel: &CancellationToken,
        _spec: TrialSpec,
        _ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        Err(Partial {
            value: None,
            error: Error::Other("no trial starts in recovery".into()),
        })
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        let mut script = self.0.borrow_mut();
        script.ended.push(id.to_owned());
        match script.compression {
            Compression::Works => Ok(()),
            Compression::Crashes => Err(Error::Crashed),
            Compression::Fails => Err(Error::Other("compress failed".into())),
        }
    }

    fn samples<'a>(&'a self, id: &'a str) -> Box<dyn Iterator<Item = TrialConditions> + 'a> {
        let script = self.0.borrow();
        if let Some(text) = script.jsonl.get(id) {
            return Box::new(ReadSamples::from_reader(Cursor::new(
                text.clone().into_bytes(),
            )));
        }
        Box::new(
            script
                .conditions
                .get(id)
                .cloned()
                .unwrap_or_default()
                .into_iter(),
        )
    }
}

struct Harness {
    mem: MemFs,
    appender: Option<Appender>,
    kernel: Rc<RefCell<KernelScript>>,
    trials: Rc<RefCell<TrialsScript>>,
    clock: Rc<RefCell<ClockState>>,
    boot: BootId,
    cancel: CancellationToken,
}

fn open(mem: &MemFs, boot: &BootId, clock: &Rc<RefCell<ClockState>>) -> Appender {
    let journal = simulated(mem)
        .open_at(boot.clone(), Timestamp::from_second(1_800_000_000).unwrap())
        .unwrap();
    Appender::resume(
        journal,
        Box::new(TestClock(Rc::clone(clock))),
        Renderer::default(),
        TimeZone::UTC,
    )
    .unwrap()
}

fn reason(kind: Option<ResetKind>) -> ResetReason {
    ResetReason {
        kind,
        raw: kind.map(|kind| format!("{kind:?}")).into_iter().collect(),
        supported: true,
    }
}

impl Harness {
    fn new(log: &Log, current: &str) -> Self {
        let mem = MemFs::new();
        let mut text = String::new();
        for event in &log.events {
            text.push_str(&event.encode().unwrap());
            text.push('\n');
        }
        mem.put("/state/events.jsonl", text);
        let mut order: Vec<BootId> = Vec::new();
        for event in &log.events {
            if !order.contains(&event.boot) {
                order.push(event.boot.clone());
            }
        }
        let current = boot(current);
        if !order.contains(&current) {
            order.push(current.clone());
        }
        let clock = Rc::new(RefCell::new(ClockState::default()));
        let appender = open(&mem, &current, &clock);
        Self {
            mem,
            appender: Some(appender),
            kernel: Rc::new(RefCell::new(KernelScript {
                order,
                ..KernelScript::default()
            })),
            trials: Rc::new(RefCell::new(TrialsScript {
                jsonl: BTreeMap::new(),
                conditions: BTreeMap::new(),
                ended: Vec::new(),
                compression: Compression::Works,
            })),
            clock,
            boot: current,
            cancel: CancellationToken::new(),
        }
    }

    fn reasons(&self, entries: &[(&str, Option<ResetKind>)]) {
        let mut script = self.kernel.borrow_mut();
        for &(name, kind) in entries {
            script.reasons.insert(boot(name), reason(kind));
        }
    }

    fn appender(&self) -> &Appender {
        self.appender.as_ref().unwrap()
    }

    fn events(&self) -> &[Event] {
        self.appender().journal().events()
    }

    fn reopen(&mut self) {
        drop(self.appender.take());
        self.appender = Some(open(&self.mem, &self.boot, &self.clock));
    }

    fn recover(&mut self) -> Step<Recovered> {
        let mut kernel = TestKernel(Rc::clone(&self.kernel));
        let mut trials = TestTrials(Rc::clone(&self.trials));
        prior_boots(Recovery {
            appender: self.appender.as_mut().unwrap(),
            kernel: &mut kernel,
            trials: &mut trials,
            boot: &self.boot,
            cancel: &self.cancel,
        })
    }

    fn same_boot(&mut self) -> Step<()> {
        let mut kernel = TestKernel(Rc::clone(&self.kernel));
        let mut trials = TestTrials(Rc::clone(&self.trials));
        let cleared = Rig::new(2).sweep;
        same_boot_trial(
            Recovery {
                appender: self.appender.as_mut().unwrap(),
                kernel: &mut kernel,
                trials: &mut trials,
                boot: &self.boot,
                cancel: &self.cancel,
            },
            &cleared,
        )
    }

    fn append_end(&mut self, end: TrialEnd) -> Step<Seq> {
        let mut trials = TestTrials(Rc::clone(&self.trials));
        append_trial_end(self.appender.as_mut().unwrap(), &mut trials, end, &[])
    }

    fn consistent(&self) {
        assert_fold_consistent(self.appender());
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.clock.borrow().sleeps.clone()
    }

    fn ended(&self) -> Vec<String> {
        self.trials.borrow().ended.clone()
    }

    fn crashes(&self) -> Vec<(&Event, &CrashDetected)> {
        self.pick(|payload| match payload {
            Payload::CrashDetected(crash) => Some(crash),
            _ => None,
        })
    }

    fn ends(&self) -> Vec<(&Event, &TrialEnd)> {
        self.pick(|payload| match payload {
            Payload::TrialEnd(end) => Some(end),
            _ => None,
        })
    }

    fn failures(&self) -> Vec<(&Event, &Failure)> {
        self.pick(|payload| match payload {
            Payload::Failure(failure) => Some(failure),
            _ => None,
        })
    }

    fn mces(&self) -> Vec<(&Event, &Mce)> {
        self.pick(|payload| match payload {
            Payload::Mce(mce) => Some(mce),
            _ => None,
        })
    }

    fn retries(&self) -> Vec<(&Event, &BackendRetry)> {
        self.pick(|payload| match payload {
            Payload::BackendRetry(retry) if retry.backend == RetryTarget::KernelLog => Some(retry),
            _ => None,
        })
    }

    fn warnings(&self) -> Vec<(&Event, &togi_journal::SessionWarning)> {
        self.pick(|payload| match payload {
            Payload::SessionWarning(warning) => Some(warning),
            _ => None,
        })
    }

    fn pick<'a, T>(
        &'a self,
        select: impl Fn(&'a Payload) -> Option<&'a T>,
    ) -> Vec<(&'a Event, &'a T)> {
        self.events()
            .iter()
            .filter_map(|event| Some((event, select(&event.payload)?)))
            .collect()
    }
}

fn base() -> Log {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    for id in [0, 8] {
        log.add(
            Payload::CorePhase(CoreTransition {
                core: core(id),
                from: None,
                to: CorePhase::Search,
                offset: target(-10),
                pass: None,
                failure_point: None,
                check_solo_limit: false,
                workloads: Vec::new(),
                cleared_combination: Vec::new(),
                reason: "test".into(),
            }),
            &[],
        );
    }
    log
}

fn trial_id(id: &str) -> TrialId {
    TrialId::from(id.to_owned())
}

fn intent(id: &str) -> Payload {
    Payload::TrialIntent(TrialIntent {
        trial: trial_id(id),
        kernel_cursor: None,
        kernel_error: None,
        profile: [(core(0), target(-10)), (core(8), TargetOffset::ZERO)]
            .into_iter()
            .collect(),
        core: Some(core(0)),
        cores: CoreSet::default(),
        offset: Some(target(-10)),
        regime: Regime::R1,
        workload: pick_workload(Regime::R1, 0).id(),
        duration_s: togi_config::Config::default().search_trial().get(),
        condition: Condition::Alone,
        activity: Some(Activity::Search),
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    })
}

fn progress(id: &str, signal: Option<Signal>) -> Payload {
    Payload::TrialProgress(TrialProgress {
        trial: trial_id(id),
        detail: "running".into(),
        signal,
        core: signal.map(|_| core(0)),
    })
}

fn open_trial(log: &mut Log, name: &str) -> Seq {
    let intent = log.add(intent(name), &[]);
    log.mono(11_000);
    let start = log.started(name, Some(11_000_000_000), intent);
    log.mono(40_000);
    log.add(progress(name, None), &[start]);
    intent
}

fn trial_log() -> Log {
    let mut log = base();
    log.at("b0", 10_000);
    open_trial(&mut log, "0001");
    log
}

fn unapplied_end(id: &str) -> TrialEnd {
    TrialEnd {
        trial: trial_id(id),
        kernel_cursor: None,
        kernel_error: None,
        outcome: Outcome::Inconclusive,
        signal: None,
        core: None,
        duration_s: 5,
        tctl_max_c: None,
        voltage_request_median_v: None,
        voltage_request_min_v: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
        last_sample_s: None,
        last_sample_tctl_c: None,
        last_sample_min_mhz: None,
        last_sample_max_mhz: None,
        stalled_core: None,
        worker_stalled_ms: None,
        reason: None,
        interrupted: true,
        last_evidence: false,
        escaped: Vec::new(),
        backend_missing: false,
        containment_error: None,
    }
}

fn written(log: &mut Log, id: i64, offset: i64) {
    let intent = log.intent(id, offset, &[]);
    let write = log.written(id, offset, intent);
    log.readback(id, offset, offset, write);
}

fn idle_log(applied: impl FnOnce(&mut Log)) -> Log {
    let mut log = base();
    log.at("b0", 20_000);
    applied(&mut log);
    log
}

fn kernel_mce(mono_s: u64, corrected: bool) -> togi_machine::Mce {
    togi_machine::Mce {
        cpu: CpuId(0),
        core: Some(core(0)),
        bank: 1,
        bank_type: BankType::LoadStore,
        corrected,
        time: Timestamp::UNIX_EPOCH,
        monotonic: Duration::from_secs(mono_s),
        lines: vec!["machine check".into()],
    }
}

fn recorded_mce(mono_s: u64, between: bool) -> Payload {
    Payload::Mce(Mce {
        cpu: CpuId(0),
        core: Some(core(0)),
        bank: 1,
        bank_type: BankType::LoadStore,
        corrected: false,
        monotonic_ns: Some(mono_s * 1_000_000_000),
        from_boot: Some(boot("b0")),
        trial: None,
        between_trials: between,
        lines: vec!["machine check".into()],
    })
}

fn crash_event(previous: &str, condition: Option<Condition>) -> Payload {
    Payload::CrashDetected(CrashDetected {
        previous_boot: boot(previous),
        in_flight: None,
        stray: false,
        condition,
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        inconclusive: false,
        pstore: None,
    })
}

fn fatal_crash(step: &Step<impl std::fmt::Debug>) -> bool {
    matches!(
        step,
        Err(Flow::Fatal(SessionError::Machine(Error::Crashed)))
    )
}

#[test]
fn an_idle_crash_is_recorded_once_and_answered_with_one_failure() {
    let mut h = Harness::new(&idle_log(|log| written(log, 0, -20)), "b1");
    h.reasons(&[("b0", None), ("b1", Some(ResetKind::Watchdog))]);
    assert_eq!(h.recover().unwrap(), Recovered::default());
    let crashes = h.crashes();
    assert_eq!(crashes.len(), 1);
    let (crash_event, crash) = crashes[0];
    assert_eq!(crash.previous_boot, boot("b0"));
    assert_eq!(crash.condition, Some(Condition::Together));
    assert!(!crash.stray && !crash.inconclusive);
    assert_eq!(crash.reset_reason, Some(ResetKind::Watchdog));
    let failures = h.failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0.cause, [crash_event.seq]);
    assert_eq!(failures[0].1.regime, Some(Regime::R6));
    assert_eq!(failures[0].1.signal, Signal::Crash);
    h.consistent();

    let before = h.events().len();
    assert_eq!(h.recover().unwrap(), Recovered::default());
    assert_eq!(h.events().len(), before);
    h.reopen();
    assert_eq!(h.recover().unwrap(), Recovered::default());
    assert_eq!(h.events().len(), before);
    h.consistent();
}

#[test]
#[expect(clippy::type_complexity, reason = "a table of cases")]
fn an_idle_failure_names_a_core_only_when_the_journal_proves_it() {
    type Applied = fn(&mut Log);
    let cases: [(&str, bool, Applied, Option<(i64, i64)>, Option<[i64; 2]>); 4] = [
        (
            "one proven nonzero core",
            true,
            |log| written(log, 0, -20),
            Some((0, -20)),
            Some([-20, 0]),
        ),
        (
            "two nonzero cores",
            true,
            |log| {
                written(log, 0, -20);
                written(log, 8, -10);
            },
            None,
            Some([-20, -10]),
        ),
        (
            "a write no readback answered",
            true,
            |log| {
                log.intent(0, -20, &[]);
            },
            None,
            Some([-20, 0]),
        ),
        (
            "a core without a baseline",
            false,
            |log| written(log, 0, -20),
            None,
            None,
        ),
    ];
    for (name, baseline, applied, sole, profile) in cases {
        let mut log = if baseline { base() } else { Log::new() };
        log.at("b0", 20_000);
        applied(&mut log);
        let mut h = Harness::new(&log, "b1");
        h.reasons(&[("b0", None), ("b1", Some(ResetKind::Watchdog))]);
        h.recover().unwrap();
        let failures = h.failures();
        assert_eq!(failures.len(), 1, "{name}");
        let failure = failures[0].1;
        assert_eq!(
            failure
                .core
                .zip(failure.offset)
                .map(|(core, offset)| (i64::from(core.get()), offset.get())),
            sole,
            "{name}"
        );
        assert_eq!(
            failure.attribution == togi_journal::Attribution::Attributed,
            sole.is_some(),
            "{name}"
        );
        let held = failure.profile.map(|profile| {
            [core(0), core(8)].map(|core| profile.get(core).map_or(i64::MIN, TargetOffset::get))
        });
        assert_eq!(held, profile, "{name}");
        h.consistent();
    }
}

#[test]
fn a_crash_with_nothing_applied_is_a_stray_and_no_failure() {
    let mut log = base();
    log.at("b0", 20_000);
    let mut h = Harness::new(&log, "b1");
    h.reasons(&[("b0", None), ("b1", Some(ResetKind::Watchdog))]);
    h.recover().unwrap();
    let crashes = h.crashes();
    assert_eq!(crashes.len(), 1);
    assert!(crashes[0].1.stray);
    assert_eq!(crashes[0].1.condition, None);
    assert!(h.failures().is_empty());
    assert_eq!(h.appender().recovery().stray(), [crashes[0].0.seq]);
    h.consistent();
}

#[test]
fn a_crash_under_an_armed_hunt_group_is_a_parked_application() {
    let log = idle_log(|log| {
        log.add(hunt_group(), &[]);
        written(log, 0, -20);
    });
    let mut h = Harness::new(&log, "b1");
    h.reasons(&[("b0", None), ("b1", Some(ResetKind::Watchdog))]);
    h.recover().unwrap();
    assert_eq!(h.crashes()[0].1.condition, Some(Condition::Parked));
    let failures = h.failures();
    assert_eq!(failures[0].1.condition, Some(Condition::Parked));
    h.consistent();
}

#[test]
fn the_reset_after_a_crash_decides_what_an_open_trial_ended_as() {
    struct Row {
        name: &'static str,
        successor: Option<ResetKind>,
        confirmed: bool,
        outcome: Outcome,
        thermal: bool,
    }
    let row = |name, successor, confirmed, outcome, thermal| Row {
        name,
        successor,
        confirmed,
        outcome,
        thermal,
    };
    let failure = Outcome::Failure;
    let rows = [
        row("watchdog", Some(ResetKind::Watchdog), true, failure, false),
        row(
            "sync flood",
            Some(ResetKind::SyncFlood),
            true,
            failure,
            false,
        ),
        row(
            "cpu shutdown",
            Some(ResetKind::CpuShutdown),
            true,
            failure,
            false,
        ),
        row(
            "power button",
            Some(ResetKind::PowerButton),
            true,
            failure,
            false,
        ),
        row(
            "a reset nothing named",
            Some(ResetKind::Unknown),
            true,
            failure,
            false,
        ),
        row(
            "power loss on a kernel that names resets",
            None,
            true,
            Outcome::Inconclusive,
            false,
        ),
        row(
            "power loss on a kernel never seen to name one",
            None,
            false,
            failure,
            false,
        ),
        row(
            "thermal trip",
            Some(ResetKind::ThermalTrip),
            true,
            Outcome::Inconclusive,
            true,
        ),
    ];
    for row in rows {
        let mut h = Harness::new(&trial_log(), "b1");
        h.reasons(&[
            ("b0", row.confirmed.then_some(ResetKind::Watchdog)),
            ("b1", row.successor),
        ]);
        h.recover().unwrap();
        let (crash_event, crash) = h.crashes()[0];
        let ends = h.ends();
        assert_eq!(ends.len(), 1, "{}", row.name);
        let (end_event, end) = ends[0];
        assert_eq!(end.outcome, row.outcome, "{}", row.name);
        assert_eq!(end_event.cause, [crash_event.seq], "{}", row.name);
        assert_eq!(
            crash.inconclusive,
            row.outcome == Outcome::Inconclusive,
            "{}",
            row.name
        );
        assert!(!crash.stray, "{}", row.name);
        assert_eq!(crash.condition, None, "{}", row.name);
        if row.outcome == Outcome::Failure {
            assert_eq!(end.signal, Some(Signal::Crash), "{}", row.name);
            assert!(!end.interrupted, "{}", row.name);
        } else {
            assert_eq!(end.signal, None, "{}", row.name);
            assert!(end.interrupted, "{}", row.name);
        }
        assert_eq!(
            h.appender().recovery().thermal_evidence().is_some(),
            row.thermal,
            "{}",
            row.name
        );
        assert!(
            h.appender().recovery().open_trial().is_none(),
            "{}",
            row.name
        );
        h.consistent();
    }
}

#[test]
fn an_idle_crash_follows_the_reset_after_it_and_only_a_failure_is_answered() {
    for (kind, inconclusive) in [
        (ResetKind::Watchdog, false),
        (ResetKind::PowerButton, true),
        (ResetKind::ThermalTrip, true),
    ] {
        let mut h = Harness::new(&idle_log(|log| written(log, 0, -20)), "b1");
        h.reasons(&[("b0", None), ("b1", Some(kind))]);
        h.recover().unwrap();
        let crash = h.crashes()[0].1;
        assert_eq!(crash.inconclusive, inconclusive, "{kind:?}");
        assert_eq!(h.failures().len(), usize::from(!inconclusive), "{kind:?}");
        h.consistent();
    }
}

#[test]
fn a_later_boot_cannot_replace_the_reset_reason_of_the_boot_that_followed_the_crash() {
    let cases = [
        (
            ResetKind::Watchdog,
            ResetKind::ThermalTrip,
            Outcome::Failure,
        ),
        (
            ResetKind::ThermalTrip,
            ResetKind::Watchdog,
            Outcome::Inconclusive,
        ),
        (
            ResetKind::PowerButton,
            ResetKind::Watchdog,
            Outcome::Failure,
        ),
    ];
    for (successor, later, outcome) in cases {
        let mut log = trial_log();
        log.at("b0", 40_000);
        let mut h = Harness::new(&log, "b2");
        h.kernel
            .borrow_mut()
            .order
            .splice(1..1, [boot("elsewhere")]);
        h.reasons(&[
            ("b0", Some(ResetKind::Watchdog)),
            ("elsewhere", Some(successor)),
            ("b2", Some(later)),
        ]);
        h.recover().unwrap();
        assert_eq!(h.crashes()[0].1.reset_reason, Some(successor));
        assert_eq!(
            h.ends()[0].1.outcome,
            outcome,
            "{successor:?} then {later:?}"
        );
    }
}

#[test]
fn a_crash_without_an_identified_successor_borrows_no_reset_reason_from_this_boot() {
    let mut h = Harness::new(&idle_log(|log| written(log, 0, -20)), "b1");
    h.kernel.borrow_mut().order = vec![boot("b0")];
    h.reasons(&[("b0", None), ("b1", Some(ResetKind::ThermalTrip))]);
    h.recover().unwrap();
    let crash = h.crashes()[0].1;
    assert_eq!((crash.reset_reason, crash.inconclusive), (None, false));
    assert!(crash.reset_reason_raw.is_empty());
    assert!(h.retries().is_empty());
    assert!(h.sleeps().is_empty());
    assert!(h.appender().recovery().thermal_evidence().is_none());
}

#[test]
fn an_uncorrected_machine_check_the_next_boot_kept_is_evidence_and_a_corrected_one_is_not() {
    for (corrected, evidence) in [(false, true), (true, false)] {
        let mut h = Harness::new(&idle_log(|log| written(log, 0, -20)), "b1");
        h.reasons(&[("b0", None), ("b1", Some(ResetKind::PowerButton))]);
        h.kernel
            .borrow_mut()
            .mces
            .insert(boot("b1"), vec![kernel_mce(3, corrected)]);
        h.recover().unwrap();
        let crash = h.crashes()[0].1;
        assert_eq!(crash.inconclusive, !evidence, "corrected {corrected}");
        assert_eq!(
            h.failures().len(),
            usize::from(evidence),
            "corrected {corrected}"
        );
        let mces = h.mces();
        assert_eq!(mces.len(), usize::from(evidence), "corrected {corrected}");
        if let Some((mce_event, mce)) = mces.first() {
            assert_eq!(mce.from_boot, Some(boot("b1")));
            assert_eq!(h.crashes()[0].0.cause, [mce_event.seq]);
            assert_eq!(
                h.failures()[0].0.cause,
                [h.crashes()[0].0.seq, mce_event.seq]
            );
        }
        h.consistent();
    }
}

#[test]
fn the_crashed_boots_own_machine_checks_are_the_trials_only_inside_its_window() {
    let mut h = Harness::new(&trial_log(), "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    h.kernel.borrow_mut().mces.insert(
        boot("b0"),
        vec![kernel_mce(5, false), kernel_mce(20, false)],
    );
    h.recover().unwrap();
    let mces = h.mces();
    assert_eq!(mces.len(), 2);
    let (before, inside) = (mces[0], mces[1]);
    assert!(before.1.between_trials && !inside.1.between_trials);
    assert_eq!(before.1.from_boot, Some(boot("b0")));
    let (crash_event, _) = h.crashes()[0];
    assert_eq!(crash_event.cause, [inside.0.seq]);
    let (end_event, end) = h.ends()[0];
    assert_eq!(end.outcome, Outcome::Failure);
    assert_eq!(end_event.cause, [crash_event.seq, inside.0.seq]);
    h.consistent();
}

#[test]
fn a_machine_check_the_journal_already_holds_is_never_recorded_or_claimed_again() {
    let mut log = trial_log();
    log.at("b0", 41_000);
    let held = log.add(recorded_mce(5, true), &[]);
    let mut h = Harness::new(&log, "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    h.kernel
        .borrow_mut()
        .mces
        .insert(boot("b0"), vec![kernel_mce(5, false)]);
    h.recover().unwrap();
    assert_eq!(h.mces().len(), 1);
    assert!(h.crashes()[0].0.cause.is_empty());
    assert!(!h.ends()[0].0.cause.contains(&held));
    let before = h.events().len();
    h.recover().unwrap();
    assert_eq!(h.events().len(), before);
    h.consistent();
}

#[test]
fn a_backend_signal_before_the_power_loss_survives_and_keeps_its_core() {
    for signal in [
        Signal::ComputationError,
        Signal::UnexpectedExit,
        Signal::Stall,
    ] {
        let mut log = base();
        log.at("b0", 10_000);
        let intent = log.add(intent("0001"), &[]);
        log.mono(11_000);
        let start = log.started("0001", Some(11_000_000_000), intent);
        log.mono(40_000);
        log.add(progress("0001", Some(signal)), &[start]);
        let mut h = Harness::new(&log, "b1");
        h.reasons(&[("b0", Some(ResetKind::Watchdog)), ("b1", None)]);
        h.recover().unwrap();
        let (_, end) = h.ends()[0];
        assert_eq!(end.outcome, Outcome::Failure, "{signal:?}");
        assert_eq!(end.signal, Some(signal), "{signal:?}");
        assert_eq!(end.core, Some(core(0)), "{signal:?}");
        assert_eq!(
            end.reason.as_deref(),
            Some("backend reported a failure before the reset"),
            "{signal:?}"
        );
        assert!(!h.crashes()[0].1.inconclusive, "{signal:?}");
        h.consistent();
    }
}

#[test]
fn a_trial_a_stopped_run_left_open_in_this_boot_ends_as_the_signal_or_the_stop_it_proves() {
    let mut signalled = base();
    signalled.at("b0", 10_000);
    let intent_seq = signalled.add(intent("0001"), &[]);
    signalled.mono(11_000);
    let start = signalled.started("0001", Some(11_000_000_000), intent_seq);
    signalled.add(progress("0001", Some(Signal::Stall)), &[start]);
    let mut h = Harness::new(&signalled, "b0");
    h.same_boot().unwrap();
    let (_, end) = h.ends()[0];
    assert_eq!(
        (end.outcome, end.signal, end.core),
        (Outcome::Failure, Some(Signal::Stall), Some(core(0)))
    );
    assert_eq!(end.reason.as_deref(), Some("togi stopped during the trial"));
    h.consistent();

    let mut h = Harness::new(&trial_log(), "b0");
    h.same_boot().unwrap();
    let (_, end) = h.ends()[0];
    assert_eq!((end.outcome, end.signal), (Outcome::Inconclusive, None));
    assert!(end.interrupted && end.last_evidence);
    assert_eq!(end.reason.as_deref(), Some("togi stopped during the trial"));
    assert_eq!(end.last_sample_s, None);
    assert!(h.crashes().is_empty());
}

#[test]
fn the_same_boot_trial_only_closes_when_it_is_this_boots_and_leaves_failures_to_a_pending_dead_end()
{
    let mut elsewhere = Harness::new(&trial_log(), "b1");
    elsewhere.same_boot().unwrap();
    assert!(elsewhere.ends().is_empty());
    assert!(elsewhere.appender().recovery().open_trial().is_some());

    let mut log = trial_log();
    log.add(crash_event("bx", Some(Condition::Together)), &[]);
    let mut answered = Harness::new(&log, "b0");
    answered.same_boot().unwrap();
    assert_eq!(answered.ends().len(), 1);
    assert_eq!(answered.failures().len(), 1);
    answered.consistent();

    log.add(dead_end(DeadEndCondition::Smu), &[]);
    let mut pending = Harness::new(&log, "b0");
    pending.same_boot().unwrap();
    assert_eq!(pending.ends().len(), 1);
    assert!(pending.failures().is_empty());
    pending.consistent();
}

#[test]
fn a_trial_open_in_this_boot_waits_for_the_sweep_while_earlier_boots_are_still_recovered() {
    let mut log = idle_log(|log| written(log, 0, -20));
    log.at("b1", 1_000);
    open_trial(&mut log, "0001");
    let mut h = Harness::new(&log, "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    h.recover().unwrap();
    assert_eq!(h.crashes().len(), 1);
    assert!(h.ends().is_empty() && h.failures().is_empty());
    assert!(h.ended().is_empty());
    assert!(h.appender().recovery().open_trial().is_some());
    h.same_boot().unwrap();
    assert_eq!(h.ends().len(), 1);
    assert_eq!(h.failures().len(), 1);
    h.consistent();
}

#[test]
fn the_kernel_log_is_retried_after_one_five_and_thirty_minutes_then_read_or_given_up_on() {
    let mut h = Harness::new(&trial_log(), "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    {
        let mut script = h.kernel.borrow_mut();
        script.failing = Some(boot("b0"));
        script.failures = 3;
    }
    assert_eq!(h.recover().unwrap(), Recovered::default());
    let waits: Vec<_> = h.retries().iter().map(|(_, retry)| retry.wait_s).collect();
    assert_eq!(waits, [60, 300, 1800]);
    let attempts: Vec<_> = h.retries().iter().map(|(_, retry)| retry.attempt).collect();
    assert_eq!(attempts, [1, 2, 3]);
    assert_eq!(
        h.sleeps(),
        [
            Duration::from_mins(1),
            Duration::from_mins(5),
            Duration::from_mins(30)
        ]
    );
    assert_eq!(h.crashes().len(), 1);
    assert!(h.appender().recovery().kernel_retries().is_empty());
    h.consistent();

    let mut h = Harness::new(&trial_log(), "b1");
    {
        let mut script = h.kernel.borrow_mut();
        script.failing = Some(boot("b0"));
        script.failures = u32::MAX;
    }
    let recovered = h.recover().unwrap();
    let retries: Vec<Seq> = h.retries().iter().map(|(event, _)| event.seq).collect();
    assert_eq!(retries.len(), 3);
    let Recovered {
        cancelled: false,
        unreadable:
            Some(KernelUnreadable {
                detail,
                retries: cited,
            }),
    } = recovered
    else {
        panic!("{recovered:?}");
    };
    assert!(detail.contains("b0"));
    assert_eq!(cited, retries);
    assert!(h.crashes().is_empty() && h.ends().is_empty());
    assert_eq!(h.sleeps().len(), 3);
}

#[test]
fn the_kernel_log_of_this_boot_is_retried_though_an_older_boot_the_system_journal_dropped_is_not() {
    let mut h = Harness::new(&trial_log(), "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    h.kernel.borrow_mut().gone.insert(boot("b0"));
    h.recover().unwrap();
    assert!(h.retries().is_empty() && h.sleeps().is_empty());
    assert_eq!(h.crashes().len(), 1);

    let mut h = Harness::new(&trial_log(), "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    h.kernel.borrow_mut().gone.insert(boot("b1"));
    let recovered = h.recover().unwrap();
    assert!(recovered.unreadable.is_some());
    assert_eq!(h.retries().len(), 3);
}

#[test]
fn a_cancelled_wait_appends_nothing_and_a_pending_retry_is_not_read_again() {
    let mut log = trial_log();
    log.at("b1", 1_000);
    log.add(
        Payload::BackendRetry(BackendRetry {
            backend: RetryTarget::KernelLog,
            attempt: 1,
            wait_s: 60,
            reason: "read the kernel log".into(),
        }),
        &[],
    );
    let mut h = Harness::new(&log, "b1");
    h.cancel.cancel();
    let before = h.events().len();
    let recovered = h.recover().unwrap();
    assert_eq!(
        recovered,
        Recovered {
            cancelled: true,
            unreadable: None
        }
    );
    assert_eq!(h.events().len(), before);
    assert!(h.kernel.borrow().reads.is_empty());
    assert!(h.sleeps().is_empty());
    assert_eq!(h.appender().recovery().kernel_retries().len(), 1);
}

#[test]
fn a_run_cut_short_in_a_retry_wait_waits_the_same_attempt_again_without_a_new_record() {
    for cut in [Error::Cancelled, Error::Crashed] {
        let mut h = Harness::new(&trial_log(), "b1");
        h.reasons(&[
            ("b0", Some(ResetKind::Watchdog)),
            ("b1", Some(ResetKind::Watchdog)),
        ]);
        {
            let mut script = h.kernel.borrow_mut();
            script.failing = Some(boot("b0"));
            script.failures = 1;
        }
        let crashed = matches!(cut, Error::Crashed);
        if crashed {
            h.clock.borrow_mut().crash_at = Some(0);
        } else {
            h.clock.borrow_mut().cancel_at = Some(0);
        }
        let first = h.recover();
        if crashed {
            assert!(fatal_crash(&first));
        } else {
            assert_eq!(
                first.unwrap(),
                Recovered {
                    cancelled: true,
                    unreadable: None
                }
            );
        }
        assert_eq!(h.retries().len(), 1);
        assert!(h.crashes().is_empty());

        h.reopen();
        h.cancel = CancellationToken::new();
        {
            let mut state = h.clock.borrow_mut();
            state.crash_at = None;
            state.cancel_at = None;
        }
        assert_eq!(h.recover().unwrap(), Recovered::default());
        assert_eq!(h.retries().len(), 1);
        assert_eq!(h.sleeps(), [Duration::from_mins(1), Duration::from_mins(1)]);
        let crashes = h.crashes();
        assert_eq!(crashes.len(), 1);
        assert!(!crashes[0].1.stray);
        assert!(h.appender().recovery().kernel_retries().is_empty());
        h.consistent();
    }
}

#[test]
fn a_machine_that_crashes_while_the_kernel_log_is_read_is_fatal_and_records_nothing() {
    let mut h = Harness::new(&trial_log(), "b1");
    h.kernel.borrow_mut().crashed = true;
    let before = h.events().len();
    assert!(fatal_crash(&h.recover()));
    assert_eq!(h.events().len(), before);
}

#[test]
fn a_saved_pstore_record_is_attached_an_absent_one_is_fine_and_an_unreadable_one_warns_once() {
    let record = PstoreRecord {
        path: "dmesg-1".into(),
        lines: vec!["Kernel panic".into()],
    };
    let mut attached = Harness::new(&trial_log(), "b1");
    attached
        .kernel
        .borrow_mut()
        .pstore
        .insert(boot("b0"), Pstore::Record(record.clone()));
    attached.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    attached.recover().unwrap();
    assert_eq!(attached.crashes()[0].1.pstore.as_ref(), Some(&record));
    assert!(attached.warnings().is_empty());

    let mut absent = Harness::new(&trial_log(), "b1");
    absent.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    absent.recover().unwrap();
    assert_eq!(absent.crashes()[0].1.pstore, None);
    assert!(absent.warnings().is_empty());

    let mut unreadable = Harness::new(&trial_log(), "b1");
    unreadable.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    unreadable
        .kernel
        .borrow_mut()
        .pstore
        .insert(boot("b0"), Pstore::Fails);
    unreadable.recover().unwrap();
    assert_eq!(unreadable.crashes()[0].1.pstore, None);
    let warnings = unreadable.warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].1.operation, "read saved pstore");
    assert_eq!(warnings[0].0.cause, [unreadable.crashes()[0].0.seq]);
    let before = unreadable.events().len();
    unreadable.recover().unwrap();
    unreadable.reopen();
    unreadable.recover().unwrap();
    assert_eq!(unreadable.events().len(), before);
    unreadable.consistent();
}

fn crashed_trial_with(samples: &str) -> Harness {
    let mut h = Harness::new(&trial_log(), "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    h.trials
        .borrow_mut()
        .jsonl
        .insert("0001".into(), samples.to_owned());
    h.recover().unwrap();
    h
}

#[test]
fn a_crashed_trials_end_carries_the_last_complete_sample_and_ignores_a_torn_one() {
    let complete = concat!(
        "{\"elapsed_ms\":1000,\"tctl_c\":70,\"core_mhz\":{\"0\":4400,\"8\":4300}}\n",
        "{\"elapsed_ms\":2000,\"tctl_c\":85,\"core_mhz\":{\"0\":4500,\"8\":4300}}\n",
    );
    for (name, text) in [
        ("complete", complete.to_owned()),
        ("torn after it", format!("{complete}{{\"elapsed_ms\":30")),
    ] {
        let h = crashed_trial_with(&text);
        let (_, end) = h.ends()[0];
        assert_eq!(end.last_sample_s, Some(2), "{name}");
        assert_eq!(end.last_sample_tctl_c, Some(85), "{name}");
        assert_eq!(end.last_sample_min_mhz, Some(4300), "{name}");
        assert_eq!(end.last_sample_max_mhz, Some(4500), "{name}");
        assert_eq!(h.ended(), ["0001"], "{name}");
    }
    let h = crashed_trial_with("");
    let (_, end) = h.ends()[0];
    assert_eq!(
        (
            end.last_sample_s,
            end.last_sample_tctl_c,
            end.last_sample_min_mhz
        ),
        (None, None, None)
    );
    assert_eq!(h.ended(), ["0001"]);
}

#[test]
fn a_recovered_trials_end_carries_the_request_telemetry_of_its_loaded_cores() {
    let samples: Vec<TrialConditions> = (0..20)
        .map(|i| {
            let mut table = PmTable::default();
            table.voltage_request_v[0] = 1.25;
            TrialConditions {
                elapsed_ms: 5000 + i * 1000,
                core_mhz: [(core(0), 5000)].into_iter().collect(),
                worker_cpu_ms: [(core(0), i * 100)].into_iter().collect(),
                pm_table: Some(table),
                ..TrialConditions::default()
            }
        })
        .collect();
    let mut h = Harness::new(&trial_log(), "b1");
    h.reasons(&[
        ("b0", Some(ResetKind::Watchdog)),
        ("b1", Some(ResetKind::Watchdog)),
    ]);
    h.trials
        .borrow_mut()
        .conditions
        .insert("0001".into(), samples);
    h.recover().unwrap();
    let (_, end) = h.ends()[0];
    assert_eq!(end.voltage_request_median_v, Some(1.25));
    assert_eq!(end.voltage_request_min_v, Some(1.25));
    assert_eq!(end.voltage_requests_v.get(core(0)), Some(1.25));
    assert!(end.top_requesters.contains(core(0)));
    assert!(!end.ccd_mhz.is_empty());
    h.consistent();
}

#[test]
fn compression_precedes_the_end_so_a_crash_between_them_leaves_the_files_to_compress_again() {
    let mut log = trial_log();
    log.add(crash_event("bx", Some(Condition::Together)), &[]);
    let mut h = Harness::new(&log, "b0");
    h.mem.inject(Fault::before(Op::Write, 1));
    assert!(matches!(
        h.same_boot(),
        Err(Flow::Fatal(SessionError::Journal(_)))
    ));
    assert_eq!(h.ended(), ["0001"]);
    assert!(h.ends().is_empty());

    h.reopen();
    h.same_boot().unwrap();
    assert_eq!(h.ended(), ["0001", "0001"]);
    assert_eq!(h.ends().len(), 1);
    assert_eq!(h.failures().len(), 1);
    h.same_boot().unwrap();
    assert_eq!(h.ends().len(), 1);
    h.consistent();
}

#[test]
fn a_failed_compression_is_a_warning_the_end_causes_and_never_changes_the_outcome() {
    let mut h = Harness::new(&trial_log(), "b0");
    h.trials.borrow_mut().compression = Compression::Fails;
    h.same_boot().unwrap();
    let (end_event, end) = h.ends()[0];
    assert_eq!(end.outcome, Outcome::Inconclusive);
    let warnings = h.warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].1.operation, "compress trial files");
    assert_eq!(warnings[0].1.trial, Some(trial_id("0001")));
    assert_eq!(warnings[0].0.cause, [end_event.seq]);
    h.consistent();
}

#[test]
fn a_machine_that_crashes_compressing_a_trial_is_fatal_and_ends_nothing() {
    let mut h = Harness::new(&trial_log(), "b0");
    h.trials.borrow_mut().compression = Compression::Crashes;
    assert!(fatal_crash(&h.same_boot()));
    assert!(h.ends().is_empty());
}

#[test]
fn a_trial_whose_containment_is_unconfirmed_keeps_its_files_as_they_are() {
    let mut h = Harness::new(&trial_log(), "b0");
    let mut end = unapplied_end("0001");
    end.containment_error = Some("backend still running".into());
    let seq = h.append_end(end).unwrap();
    assert!(h.ended().is_empty());
    assert_eq!(h.ends()[0].0.seq, seq);

    let mut h = Harness::new(&trial_log(), "b0");
    h.append_end(unapplied_end("0001")).unwrap();
    assert_eq!(h.ended(), ["0001"]);
    h.consistent();
}
