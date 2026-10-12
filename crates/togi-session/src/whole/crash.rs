use std::cell::Cell;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Duration;

use togi_config::Config;
use togi_journal::fs::{Fault, Op};
use togi_journal::{
    Activity, Attribution, Decision, Event, Outcome, Payload, Seq, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{
    CancellationToken, Condition, CoreSet, Error, Partial, PerCore, Regime, Running, Signal,
    Stopped, Swept, TargetOffset, TrialSpec, Trials,
};
use togi_sim::MachineDefinition;
use togi_sim::definition::{CoreDef, ModelDef, Weights};
use togi_sim::faults::SmuOperation;
use togi_sim::model::Script;
use togi_tuner::{Constraints, CoreView, Profile};

use crate::restore::restore_target;
use crate::testing::{Bench, simulated};
use crate::{Ended, Response, Stop, Withheld, run};

const BOOTS: usize = 200;

fn limits(id: i64, alone: [i64; 5]) -> CoreDef {
    let deepest = alone.iter().copied().max().unwrap();
    CoreDef {
        id,
        alone,
        together: [
            alone[0], alone[1], alone[2], alone[3], alone[4], deepest, deepest,
        ],
        flat: 0.0,
        idle: None,
        workload: BTreeMap::new(),
    }
}

fn definition(model: ModelDef) -> MachineDefinition {
    MachineDefinition {
        cores: 2,
        bios: Some(vec![-10, -10]),
        core: vec![
            limits(0, [-12, -12, -12, -12, -12]),
            limits(1, [-13, -13, -13, -11, -13]),
        ],
        model,
        ..MachineDefinition::default()
    }
}

fn bench_of(model: ModelDef) -> Bench {
    let mut bench = Bench::defined(&definition(model));
    bench.config = Config::parse(r#"{"evidence":{"rate":0.95,"miss":0.2}}"#).unwrap();
    bench
}

fn bench() -> Bench {
    bench_of(ModelDef::default())
}

fn crashing() -> Bench {
    bench_of(ModelDef {
        signals: Weights::from([(Signal::Crash, 1.0)]),
        crash_mce: 0.0,
        ..ModelDef::default()
    })
}

fn one_cycle(bench: &Bench) -> RunEnd {
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    let base = input.journal.events().len();
    RunEnd {
        ended: run(input),
        base,
    }
}

struct RunEnd {
    ended: Ended,
    base: usize,
}

impl RunEnd {
    fn finished(&self) -> bool {
        self.ended.stop.is_some() && self.ended.errors.is_empty()
    }
}

/// Runs until a run stops cleanly: a run that crashed starts again after a reboot.
fn drive(bench: &Bench) -> Ended {
    for _ in 0..BOOTS {
        let end = one_cycle(bench);
        if end.finished() {
            return end.ended;
        }
        assert!(end.ended.crashed(), "a run ended in {:?}", end.ended.errors);
        bench.sim.reboot();
    }
    panic!("no stop after {BOOTS} boots");
}

#[derive(Clone, Copy)]
enum Fate {
    /// The process dies on the same boot.
    Killed,
    /// The machine crashes and boots again.
    Crashed,
}

/// Drives like [`drive`], but the journal's `after`th event is the last one the first run to reach it makes
/// durable: that run's append fails right after its line is synced, then `fate` befalls the machine.
fn interrupted(bench: &Bench, after: usize, fate: Fate) -> Ended {
    let mut fired = false;
    for _ in 0..BOOTS {
        let mut input = bench.input();
        input.policy.cycles = Some(NonZeroU32::MIN);
        let base = input.journal.events().len();
        let armed = !fired && after > base;
        if armed {
            let nth = u64::try_from(after - base).unwrap();
            bench
                .mem
                .inject(Fault::before(Op::Sync, nth).named("events.jsonl"));
        }
        let ended = run(input);
        bench.mem.clear_faults();
        let end = RunEnd { ended, base };
        if armed && end.ended.capsule.is_none() {
            drop(end);
            assert_eq!(durable(bench).len(), after);
            fired = true;
            if matches!(fate, Fate::Crashed) {
                bench.sim.crash();
                bench.sim.reboot();
            }
            continue;
        }
        if end.finished() {
            assert!(
                fired || after <= end.base,
                "event #{after} was never reached"
            );
            return end.ended;
        }
        assert!(end.ended.crashed(), "a run ended in {:?}", end.ended.errors);
        bench.sim.reboot();
    }
    panic!("no stop after {BOOTS} boots");
}

fn durable(bench: &Bench) -> Vec<Event> {
    let boot = bench.sim.seams().host.boot_id().unwrap();
    simulated(&bench.mem)
        .open_at(boot, bench.sim.now())
        .unwrap()
        .events()
        .to_vec()
}

/// Runs `check` for each of `points` on as many threads as the machine offers, up to eight.
fn each(points: &[usize], check: impl Fn(usize) + Sync) {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let check = &check;
                scope.spawn(move || {
                    points
                        .iter()
                        .copied()
                        .skip(worker)
                        .step_by(workers)
                        .for_each(check);
                })
            })
            .collect();
        for handle in handles {
            if let Err(panic) = handle.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
}

#[derive(Debug, PartialEq)]
struct Summary {
    cores: Vec<CoreView>,
    clean_cycles: u32,
    cycle: Option<NonZeroU32>,
    dead_end: bool,
}

fn summary(events: &[Event]) -> Summary {
    let facts = togi_journal::State::replay(events).unwrap();
    let projection = togi_tuner::replay(events).unwrap().project();
    Summary {
        cores: projection.cores,
        clean_cycles: projection.clean_cycles,
        cycle: projection.checking.cycle,
        dead_end: facts.dead_end.is_some(),
    }
}

/// The journal folds, nothing it opened is left open, the machine holds the safe target the restoration bounds, and
/// no trial ran alone beside a nonzero offset.
fn assert_settled(bench: &Bench, ended: &Ended) {
    let events = ended.journal.events();
    let facts = togi_journal::State::replay(events).unwrap();
    assert!(facts.open.is_empty(), "{:?}", facts.open);
    let tuner = togi_tuner::replay(events).unwrap();
    let held: PerCore<TargetOffset> = tuner
        .project()
        .cores
        .iter()
        .map(|view| (view.core, view.offset))
        .collect();
    let baseline = events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::SessionBaseline(baseline) => Some(baseline.offsets),
            _ => None,
        })
        .unwrap();
    let cores: CoreSet = held.iter().map(|(core, _)| core).collect();
    let safe = restore_target(
        &baseline,
        &held,
        tuner.constraints().map(Constraints::failure_points),
        cores,
    );
    let mut smu = bench.sim.seams().smu;
    for (core, target) in safe.iter() {
        assert_eq!(smu.offset(core).unwrap(), target.observed(), "core {core}");
    }
    assert!(
        bench.sim.violations().is_empty(),
        "{:?}",
        bench.sim.violations()
    );
}

fn reference(bench: &Bench) -> Ended {
    let ended = drive(bench);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    assert_settled(bench, &ended);
    ended
}

fn all_points(events: &[Event]) -> Vec<usize> {
    (1..=events.len()).collect()
}

#[test]
fn killing_the_run_at_every_durable_event_resumes_to_the_same_final_state() {
    let reference = reference(&bench());
    let events = reference.journal.events();
    let want = summary(events);
    let first_start = events
        .iter()
        .position(|event| matches!(event.payload, Payload::TrialStart(_)))
        .unwrap()
        + 1;
    let Payload::TrialStart(first) = &events[first_start - 1].payload else {
        unreachable!()
    };
    each(&all_points(events), |k| {
        let bench = bench();
        let ended = interrupted(&bench, k, Fate::Killed);
        assert_eq!(ended.stop, Some(Stop::Cycles), "killed at #{k}");
        assert_settled(&bench, &ended);
        assert_eq!(summary(ended.journal.events()), want, "killed at #{k}");
        if k == first_start {
            let end = ended
                .journal
                .events()
                .iter()
                .find_map(|event| match &event.payload {
                    Payload::TrialEnd(end) if end.trial == first.trial => Some(end),
                    _ => None,
                })
                .unwrap();
            assert!(end.interrupted, "{end:?}");
            assert_eq!(end.outcome, Outcome::Inconclusive);
            assert_eq!(end.duration_s, 0);
        }
    });
}

/// The seqs from each trial's intent through its end on the boot that intended it, and the failure that follows a
/// failed end.
fn trial_events(events: &[Event]) -> Vec<usize> {
    let mut positions = Vec::new();
    for (at, event) in events.iter().enumerate() {
        let Payload::TrialIntent(intent) = &event.payload else {
            continue;
        };
        for (offset, next) in events[at..].iter().enumerate() {
            if next.boot != event.boot {
                break;
            }
            let k = at + offset + 1;
            positions.push(k);
            if let Payload::TrialEnd(end) = &next.payload
                && end.trial == intent.trial
            {
                if end.outcome == Outcome::Failure
                    && matches!(events.get(k).map(|e| &e.payload), Some(Payload::Failure(_)))
                {
                    positions.push(k + 1);
                }
                break;
            }
        }
    }
    positions.sort_unstable();
    positions.dedup();
    positions
}

fn crash_detected_for(events: &[Event], boot: &togi_machine::BootId) -> Option<Seq> {
    events.iter().find_map(|event| match &event.payload {
        Payload::CrashDetected(crash) if crash.previous_boot == *boot => Some(event.seq),
        _ => None,
    })
}

fn assert_no_together_intent_reaches_a_constraint(events: &[Event]) {
    let mut tuner = togi_tuner::State::new();
    for event in events {
        if let Payload::TrialIntent(intent) = &event.payload
            && intent.condition != Condition::Alone
            && let (Some(topology), Some(constraints)) = (tuner.topology(), tuner.constraints())
        {
            let profile = Profile::new(topology, intent.profile).unwrap();
            assert_eq!(
                constraints.reaches(&profile).unwrap(),
                None,
                "trial {}",
                intent.trial
            );
        }
        tuner.fold(event).unwrap();
    }
}

fn assert_crash_is_accounted_for(events: &[Event], k: usize) {
    let hit = &events[k - 1];
    let detected = crash_detected_for(events, &hit.boot)
        .unwrap_or_else(|| panic!("no crash.detected for the boot of #{k}"));
    let mut open: Option<&TrialIntent> = None;
    let mut durable_end: Option<&TrialEnd> = None;
    let mut started: Option<Duration> = None;
    let mut last: Option<Duration> = None;
    for event in &events[..k] {
        match &event.payload {
            Payload::TrialIntent(intent) => {
                open = Some(intent);
                durable_end = None;
                started = None;
                last = None;
            }
            Payload::TrialStart(_) => {
                started = event.mono;
                last = event.mono;
            }
            Payload::TrialProgress(_) | Payload::TrialSignal(_) | Payload::TrialSample(_)
                if started.is_some() =>
            {
                last = event.mono;
            }
            Payload::TrialEnd(end) if open.is_some_and(|intent| intent.trial == end.trial) => {
                durable_end = Some(end);
            }
            _ => {}
        }
    }
    let intent = open.unwrap_or_else(|| panic!("#{k} has no trial.intent before it"));
    let ended_by_crash = durable_end.is_none();
    if ended_by_crash {
        let closed = events[k..]
            .iter()
            .find_map(|event| match &event.payload {
                Payload::TrialEnd(end) if end.trial == intent.trial => Some(end),
                _ => None,
            })
            .unwrap_or_else(|| panic!("trial {} left open after #{k}", intent.trial));
        let ran = match (started, last) {
            (Some(started), Some(last)) => last.saturating_sub(started),
            _ => Duration::ZERO,
        };
        assert_eq!(
            u64::from(closed.duration_s),
            ran.as_secs(),
            "crash at #{k}: {closed:?}"
        );
        assert_eq!(closed.outcome, Outcome::Failure, "{closed:?}");
    }
    let failure = if ended_by_crash
        || durable_end.is_some_and(|end| end.outcome == Outcome::Failure)
    {
        let citing = events.iter().find_map(|event| match &event.payload {
            Payload::Failure(failure) if event.cause.contains(&detected) => Some(failure),
            _ => None,
        });
        Some(citing.unwrap_or_else(|| {
            let same: Vec<_> = events
                .iter()
                .filter_map(|event| match &event.payload {
                    Payload::Failure(failure) if failure.trial.as_ref() == Some(&intent.trial) => {
                        Some(failure)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(same.len(), 1, "crash at #{k}: {same:?}");
            same[0]
        }))
    } else {
        None
    };
    let Some(failure) = failure.filter(|_| ended_by_crash) else {
        return;
    };
    assert_eq!(failure.trial.as_ref(), Some(&intent.trial), "#{k}");
    match intent.condition {
        Condition::Alone => {
            assert_eq!(failure.core, intent.core, "#{k}");
            assert_eq!(failure.offset, intent.offset, "#{k}");
        }
        Condition::Together if failure.signal == Signal::Crash => {
            assert_eq!(failure.attribution, Attribution::Unattributed, "#{k}");
        }
        _ => {}
    }
}

#[test]
fn a_crash_at_every_trial_event_leaves_a_resumable_journal() {
    let reference = reference(&bench());
    each(&trial_events(reference.journal.events()), |k| {
        let bench = bench();
        let ended = interrupted(&bench, k, Fate::Crashed);
        assert_eq!(ended.stop, Some(Stop::Cycles), "crashed at #{k}");
        assert_settled(&bench, &ended);
        let events = ended.journal.events();
        assert_crash_is_accounted_for(events, k);
        assert_no_together_intent_reaches_a_constraint(events);
    });
}

#[test]
fn a_crash_during_recovery_fails_the_original_trial_once_and_the_recovery_boot_is_stray() {
    let reference = reference(&crashing());
    let events = reference.journal.events();
    let first = events
        .iter()
        .position(|event| matches!(event.payload, Payload::CrashDetected(_)))
        .expect("the reference run never crashed");
    let recovery_boot = events[first].boot.clone();
    let Payload::CrashDetected(detected) = &events[first].payload else {
        unreachable!()
    };
    let in_flight = usize::try_from(detected.in_flight.unwrap().get()).unwrap();
    let Payload::TrialIntent(crashed) = &events[in_flight - 1].payload else {
        panic!("the crash found no trial in flight");
    };
    let crashed_trial = crashed.trial.clone();
    let points: Vec<usize> = events
        .iter()
        .take_while(|event| {
            !matches!(event.payload, Payload::ProfileApplied(_)) || event.boot != recovery_boot
        })
        .filter(|event| event.boot == recovery_boot)
        .map(|event| usize::try_from(event.seq.get()).unwrap())
        .collect();
    assert!(!points.is_empty());
    each(&points, |k| {
        let bench = crashing();
        let ended = interrupted(&bench, k, Fate::Crashed);
        assert_eq!(ended.stop, Some(Stop::Cycles), "crashed at #{k}");
        let events = ended.journal.events();
        let failures: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.payload {
                Payload::Failure(failure) if failure.trial.as_ref() == Some(&crashed_trial) => {
                    Some(failure)
                }
                _ => None,
            })
            .collect();
        assert_eq!(failures.len(), 1, "crashed at #{k}: {failures:?}");
        assert_eq!(failures[0].attribution, Attribution::Attributed, "#{k}");
        assert_eq!(failures[0].signal, Signal::Crash, "#{k}");
        let stray = events.iter().any(|event| {
            matches!(&event.payload, Payload::CrashDetected(crash)
                if crash.previous_boot == recovery_boot && crash.stray)
        });
        assert!(
            stray,
            "no stray crash.detected for the recovery boot, crashed at #{k}"
        );
    });
}

fn stray_crashes(ended: &Ended) -> usize {
    ended
        .journal
        .events()
        .iter()
        .filter(|event| matches!(&event.payload, Payload::CrashDetected(crash) if crash.stray))
        .count()
}

#[test]
fn fewer_stray_crashes_than_the_threshold_do_not_stop_the_run() {
    let bench = bench();
    bench.sim.crash_before_apply(2);
    let ended = drive(&bench);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    assert_eq!(stray_crashes(&ended), 2);
    assert_settled(&bench, &ended);
}

fn r7_bench() -> Bench {
    let cores = 8;
    let mut bench = Bench::defined(&MachineDefinition {
        cores,
        bios: Some(vec![0; 8]),
        core: (0..cores)
            .map(|id| CoreDef {
                id,
                alone: [-50; 5],
                together: [-50; 7],
                flat: 0.0,
                idle: None,
                workload: BTreeMap::new(),
            })
            .collect(),
        model: ModelDef {
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    });
    bench.config = Config::parse(
        r#"{
            "candidate_solo_limits": {"0":-20,"1":-25,"2":-30,"3":-35,"4":-20,"5":-25,"6":-30,"7":-35},
            "durations": {"short_trial_s":1,"checking_trial_s":1,"checking_all_core_s":4},
            "evidence": {"rate":0.95,"miss":0.2}
        }"#,
    )
    .unwrap();
    bench
}

fn first_partial(events: &[Event]) -> &TrialIntent {
    events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::TrialIntent(intent)
                if intent.regime == Regime::R7
                    && (2..4).contains(&intent.cores.iter().count())
                    && intent.condition == Condition::Together =>
            {
                Some(intent)
            }
            _ => None,
        })
        .expect("the reference run never ran an R7 partial")
}

struct CancelAfterAnotherTrial {
    inner: Box<dyn Trials>,
    cancel: CancellationToken,
    recovered: String,
}

impl Trials for CancelAfterAnotherTrial {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        stopped: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        self.inner.start(cancel, spec, stopped)
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)?;
        if id != self.recovered {
            self.cancel.cancel();
        }
        Ok(())
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.inner.samples(id)
    }
}

fn trial_end<'a>(events: &'a [Event], trial: &TrialId) -> &'a togi_journal::TrialEnd {
    events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::TrialEnd(end) if end.trial == *trial => Some(end),
            _ => None,
        })
        .unwrap()
}

#[test]
fn a_partial_crash_recovers_into_the_named_or_loaded_core_backoff_through_run() {
    let reference = drive(&r7_bench());
    let partial = first_partial(reference.journal.events()).clone();
    let top = partial
        .cores
        .iter()
        .map(|core| partial.profile.get(core).unwrap())
        .max()
        .unwrap();
    let named = partial
        .cores
        .iter()
        .min_by_key(|core| partial.profile.get(*core).unwrap())
        .unwrap();
    assert!(
        partial.profile.get(named).unwrap() < top,
        "{:?}",
        partial.profile
    );
    for signal in [None, Some(Signal::ComputationError)] {
        let bench = r7_bench();
        bench
            .sim
            .script_trial(
                partial.trial.as_str(),
                Script {
                    signal: Some(signal.unwrap_or(Signal::Crash)),
                    at_s: 1.0,
                    core: named,
                    reset: None,
                    then_crash: true,
                },
            )
            .unwrap();
        let mut before = bench.input();
        before.policy.cycles = Some(NonZeroU32::MIN);
        let crashed = run(before);
        assert!(crashed.crashed(), "{:?}", crashed.errors);
        drop(crashed);
        bench.sim.reboot();

        let mut resumed = bench.input();
        resumed.policy.cycles = Some(NonZeroU32::MIN);
        resumed.machine.trials = Box::new(CancelAfterAnotherTrial {
            inner: resumed.machine.trials,
            cancel: resumed.cancel.clone(),
            recovered: partial.trial.to_string(),
        });
        let ended = run(resumed);
        assert!(ended.errors.is_empty(), "{:?}", ended.errors);
        assert_eq!(ended.stop, Some(Stop::Signal));
        let events = ended.journal.events();
        let end = trial_end(events, &partial.trial);
        assert_eq!(end.outcome, Outcome::Failure, "{signal:?}");
        let failure = events
            .iter()
            .find_map(|event| match &event.payload {
                Payload::Failure(failure) if failure.trial.as_ref() == Some(&partial.trial) => {
                    Some(failure)
                }
                _ => None,
            })
            .expect("no recovered failure");
        let after_end = events
            .iter()
            .position(|event| matches!(&event.payload, Payload::TrialEnd(end) if end.trial == partial.trial))
            .unwrap();
        let next = events[after_end..]
            .iter()
            .find_map(|event| match &event.payload {
                Payload::TrialIntent(intent) => Some(intent),
                _ => None,
            })
            .expect("no trial after the recovered partial");
        let backoff = events.iter().find_map(|event| match &event.payload {
            Payload::TunerDecision(decision)
                if decision.decision == Decision::Backoff
                    && decision.activity != Activity::Search =>
            {
                Some(decision)
            }
            _ => None,
        });
        let hunted = events
            .iter()
            .any(|event| matches!(event.payload, Payload::HuntStart(_)));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.payload, Payload::Combination(_))),
            "an R7 partial combined cores"
        );
        assert!(!hunted, "{signal:?}: a hunt started before the backoff");
        let backoff = backoff.expect("no checking backoff");
        if let Some(signal) = signal {
            assert_ne!(
                partial.profile, next.profile,
                "the failed partial did not back off"
            );
            assert_eq!(end.signal, Some(signal));
            assert_eq!(end.core, Some(named));
            assert_eq!(failure.attribution, Attribution::Attributed);
            assert_eq!(failure.core, Some(named));
            assert_eq!(backoff.core, named);
        } else {
            assert_eq!(end.signal, Some(Signal::Crash));
            assert_eq!(failure.attribution, Attribution::Unattributed);
            assert!(partial.cores.contains(backoff.core), "{backoff:?}");
            assert!(
                backoff
                    .reason
                    .contains("not located on the unloaded cores yet"),
                "{}",
                backoff.reason
            );
            assert!(next.rerun, "{next:?}");
            assert_eq!(next.condition, Condition::Together);
            assert_eq!(next.cores, partial.cores);
        }
    }
}

struct Watched {
    inner: Box<dyn Trials>,
    swept: Rc<Cell<bool>>,
    started: Rc<Cell<u32>>,
    stopped: Rc<Cell<u32>>,
}

struct WatchedRunning {
    inner: Box<dyn Running>,
    stopped: Rc<Cell<u32>>,
}

impl Running for WatchedRunning {
    fn started(&self) -> togi_machine::Started {
        self.inner.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn togi_machine::Reporter,
    ) -> Result<togi_machine::TrialResult, Partial<togi_machine::TrialResult>> {
        self.inner.wait(cancel, report)
    }

    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        let stopped = self.inner.stop()?;
        self.stopped.set(self.stopped.get() + 1);
        Ok(stopped)
    }
}

impl Trials for Watched {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        let swept = self.inner.sweep(cancel)?;
        self.swept.set(true);
        Ok(swept)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        stopped: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let running = self.inner.start(cancel, spec, stopped)?;
        self.started.set(self.started.get() + 1);
        Ok(Box::new(WatchedRunning {
            inner: running,
            stopped: Rc::clone(&self.stopped),
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.inner.samples(id)
    }
}

fn scripted() -> Bench {
    let bench = bench();
    bench
        .sim
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: crate::testing::core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    bench
}

#[test]
fn every_early_append_failure_respects_the_sweep_gate() {
    let reference = drive(&scripted());
    let events = reference.journal.events();
    let sweep = events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::PreflightCheck(check) if check.check == "trial_scopes")
        })
        .unwrap()
        + 1;
    let progress = events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::TrialProgress(progress)
                if progress.signal == Some(Signal::ComputationError))
        })
        .unwrap()
        + 1;
    let kinds: Vec<_> = events.iter().map(|event| event.kind().as_str()).collect();
    let points: Vec<usize> = (1..=progress).collect();
    each(&points, |k| {
        let bench = scripted();
        let swept = Rc::new(Cell::new(false));
        let touched_early = Rc::new(Cell::new(false));
        let (started, stopped) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let (gate, early) = (Rc::clone(&swept), Rc::clone(&touched_early));
        bench.sim.interrupt_smu(move |_: &SmuOperation| {
            early.set(early.get() || !gate.get());
            Ok(())
        });
        let mut input = bench.input();
        input.policy.cycles = Some(NonZeroU32::MIN);
        input.machine.trials = Box::new(Watched {
            inner: input.machine.trials,
            swept: Rc::clone(&swept),
            started: Rc::clone(&started),
            stopped: Rc::clone(&stopped),
        });
        let nth = u64::try_from(k).unwrap();
        bench
            .mem
            .inject(Fault::before(Op::Write, nth).named("events.jsonl"));
        let result = run(input);
        assert!(
            format!("{:?}", result.errors).contains("injected Write fault"),
            "append #{k}: {:?}",
            result.errors
        );
        assert!(
            !touched_early.get(),
            "append #{k}: the SMU was touched before the sweep"
        );
        assert_eq!(
            started.get(),
            stopped.get(),
            "append #{k}: a trial backend may still run"
        );
        let emergency = result.emergency.as_ref().unwrap();
        let baseline = if k < sweep { -10 } else { 0 };
        match &emergency.response {
            Response::Withheld(Withheld::NoSweep) if k < sweep => {}
            Response::Zeroed(zeroed) if k >= sweep => {
                assert!(zeroed.is_clean(), "append #{k}: {zeroed:?}");
            }
            other => panic!("append #{k} (sweep at #{sweep}): {other:?}"),
        }
        let mut smu = bench.sim.seams().smu;
        for core in 0..2 {
            let offset = smu.offset(crate::testing::core(core)).unwrap();
            assert_eq!(offset.0, baseline, "append #{k}, core {core}");
        }
        assert!(bench.sim.violations().is_empty());
        drop(result);
        let written = durable(&bench);
        assert_eq!(written.len(), k - 1, "append #{k}");
        for (at, event) in written.iter().enumerate() {
            assert_eq!(
                event.kind().as_str(),
                kinds[at],
                "append #{k}, event #{}",
                at + 1
            );
        }
    });
}
