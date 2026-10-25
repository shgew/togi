//! Simulated trials: no process runs; a trial's outcome is a recorded fact if the replay has one, else a
//! scripted outcome, else the earliest of the model's competing exponential failure draws.

use rand::RngExt;
use rand::rngs::StdRng;
use std::io;
use std::time::Duration;
use togi_machine::{
    CancellationToken, Condition, ContentId, CoreId, CoreSet, CpuId, Error, LoadSchedule, Partial,
    Regime, Reporter, ResetKind, Running, Signal, Started, Stopped, Swept, TargetOffset,
    TrialConditions, TrialFailure, TrialResult, TrialSpec, Trials, WorkloadId,
};

use crate::hazard::{HazardSource, Load, SourceKind};
use crate::random::{Domain, stream, text_part};
use crate::replay::ReplayOutcome;
use crate::runtime::{ArithmeticError, PendingMce, Runtime, Shared};
use crate::samples::LastTrial;
use crate::vocab::regime_key;

const BURST: Duration = Duration::from_millis(100);
const BURST_EVERY: Duration = Duration::from_secs(2);

fn other(message: impl Into<String>) -> Error {
    Error::Other(message.into().into())
}

#[expect(
    clippy::disallowed_methods,
    reason = "the simulator is a seam: no workload ever runs, so teardown is always confirmed"
)]
fn confirmed() -> Stopped {
    Stopped::confirmed()
}

fn load_of(spec: &TrialSpec) -> Load<'_> {
    Load {
        regime: spec.regime,
        workload: spec.workload.as_str(),
        cores: spec.cores.iter().copied().collect(),
        duration: spec.duration,
    }
}

pub(crate) struct SimTrials(pub(crate) Shared);

impl Trials for SimTrials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(Swept {
            detail: "simulated machine has no leftover trial scopes".to_owned(),
            stopped: confirmed(),
        })
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        drop(ready);
        let fail = |error| Partial {
            value: Some(confirmed()),
            error,
        };
        let mut runtime = self.0.borrow_mut();
        runtime.alive().map_err(fail)?;
        if cancel.is_cancelled() {
            return Err(fail(Error::Cancelled));
        }
        if runtime.faults.fail_setup > 0 {
            runtime.faults.fail_setup -= 1;
            return Err(fail(other("simulated backend setup failure")));
        }
        if spec.roles.iter().any(|role| {
            runtime
                .faults
                .failing_backends
                .contains(&role.workload.backend())
        }) {
            return Err(fail(other("simulated backend setup failure")));
        }
        if runtime
            .faults
            .missing_backends
            .contains(&spec.workload.backend())
        {
            return Err(fail(Error::BackendMissing));
        }
        if let Some(core) = spec
            .cores
            .iter()
            .find(|core| core.index() >= runtime.regs.len())
        {
            return Err(fail(other(format!(
                "trial {}: core {core} does not exist",
                spec.id
            ))));
        }
        if spec.condition == Condition::Alone {
            let loaded: CoreSet = spec.cores.iter().copied().collect();
            let outsiders: Vec<String> = crate::cores::ids(runtime.regs.len())
                .filter(|&core| !loaded.contains(core) && runtime.regs[core.index()].get() != 0)
                .map(|core| {
                    format!(
                        "trial {}: core {core} at {} during a trial alone on cores {:?}",
                        spec.id,
                        runtime.regs[core.index()].get(),
                        spec.cores.iter().map(|c| c.get()).collect::<Vec<_>>()
                    )
                })
                .collect();
            runtime.violations.extend(outsiders);
        }
        let started = Started {
            pid: saturating_trial_pid(spec.id.as_str()),
            scope: format!("togi-trial-{}", spec.id),
            cpus: spec.cpus.clone(),
            argv: vec!["sim".to_owned(), spec.workload.to_string()],
            schedule: LoadSchedule::for_spec(&spec),
            ..Started::default()
        };
        let escape = std::mem::take(&mut runtime.faults.escape);
        let boot = runtime.boot_number;
        drop(runtime);
        Ok(Box::new(SimRunning {
            shared: self.0.clone(),
            boot,
            spec,
            started,
            escape,
            state: State::Started,
        }))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "simulation output is disposable and never requests durability"
    )]
    fn ended(&mut self, id: &str) -> Result<(), Error> {
        let runtime = self.0.borrow();
        if let Some(storage) = &runtime.sample_storage {
            return storage.ended(id);
        }
        let Some(dir) = &runtime.samples_dir else {
            return Ok(());
        };
        togi_trialfiles::compress_unsynced(&dir.join(id))
            .map_err(|errors| Error::Other(Box::new(errors)))
    }

    fn content(&mut self, workload: WorkloadId) -> Result<ContentId, Error> {
        if self
            .0
            .borrow()
            .faults
            .missing_backends
            .contains(&workload.backend())
        {
            return Err(Error::BackendMissing);
        }
        Ok(crate::content::content_id(workload))
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, togi_machine::SampleError>> + 'a> {
        let runtime = self.0.borrow();
        if let Some(storage) = &runtime.sample_storage {
            return storage.read(id);
        }
        match &runtime.samples_dir {
            Some(dir) => Box::new(crate::samples::read(dir, id)),
            None if runtime.samples.id.as_deref() == Some(id) => {
                Box::new(runtime.samples.conditions().map(Ok))
            }
            None => Box::new(std::iter::empty()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Started,
    Waited,
}

struct SimRunning {
    shared: Shared,
    boot: u32,
    spec: TrialSpec,
    started: Started,
    escape: bool,
    state: State,
}

impl SimRunning {
    fn stale(&self) -> bool {
        let runtime = self.shared.borrow();
        runtime.crashed || runtime.boot_number != self.boot
    }
}

impl Running for SimRunning {
    fn started(&self) -> Started {
        self.started.clone()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        let fail = |error| Partial {
            value: TrialResult::default(),
            error,
        };
        if self.stale() {
            return Err(fail(Error::Crashed));
        }
        if self.state == State::Waited {
            return Err(fail(other("the trial was already waited on")));
        }
        self.state = State::Waited;
        if cancel.is_cancelled() {
            return Err(fail(Error::Cancelled));
        }
        let outcome = self
            .shared
            .borrow_mut()
            .run(&self.spec, self.escape)
            .map_err(fail)?;
        for event in outcome.events {
            match event {
                Event::Progress(detail) => report.progress(&detail),
                Event::Signal(core, signal, detail) => report.signal(core, signal, None, &detail),
            }
        }
        match outcome.result {
            Err(partial) => Err(Partial {
                value: partial,
                error: Error::Crashed,
            }),
            Ok(result) => match outcome.persisted {
                Ok(()) => Ok(result),
                Err(error) => Err(Partial {
                    value: result,
                    error: Error::Other(Box::new(error)),
                }),
            },
        }
    }

    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        if self.stale() {
            Err(Error::Crashed)
        } else {
            Ok(confirmed())
        }
    }
}

/// What the reporter is told, replayed after the trial's effects on the machine are applied so that no
/// runtime borrow is held across a reporter call.
enum Event {
    Progress(String),
    Signal(CoreId, Signal, String),
}

struct Outcome {
    /// `Err` is a crash, carrying what the trial accumulated.
    result: Result<TrialResult, TrialResult>,
    events: Vec<Event>,
    persisted: io::Result<()>,
}

/// What a crash leaves behind as machine-check evidence.
#[derive(Clone, Copy, Debug)]
enum Evidence {
    /// A per-core crash: an uncorrected MCE with the model's probability.
    Core,
    /// None: unloaded cores, the shared rail, CCD residuals, background and unnamed joints.
    Nothing,
    /// A joint's explicit `crash_mce_core`, whether or not that core is a member or loaded.
    Named(CoreId),
}

/// The first failure a trial meets.
struct Failure {
    core: CoreId,
    at: Duration,
    /// Overrides the signal drawn for the failure, though that draw is still taken.
    signal: Option<Signal>,
    evidence: Evidence,
    background: bool,
    reset: Option<ResetKind>,
    then_crash: bool,
    replayed: bool,
}

impl Failure {
    const fn new(core: CoreId, at: Duration) -> Self {
        Self {
            core,
            at,
            signal: None,
            evidence: Evidence::Core,
            background: false,
            reset: None,
            then_crash: false,
            replayed: false,
        }
    }
}

const fn condition_index(condition: Condition) -> u64 {
    match condition {
        Condition::Alone => 0,
        Condition::Together => 1,
        Condition::Parked => 2,
    }
}

fn exp1(rng: &mut StdRng) -> f64 {
    -(-rng.random::<f64>()).ln_1p()
}

/// The words of a draw key: five, or six for an unloaded core. The length is part of the stream, so
/// only the active words are read.
struct DrawKey {
    words: [u64; 6],
    len: usize,
}

impl DrawKey {
    fn words(&self) -> &[u64] {
        &self.words[..self.len]
    }
}

impl Runtime {
    /// The key of every draw about `core` in a trial. It names the core, regime, condition, the core's
    /// applied offset and the trial index, but not the trial ID or the other cores' offsets, so a retry
    /// of the same trial reproduces its class outcome. An unloaded core's draws also name the loaded set,
    /// as a set, so their order never matters.
    fn key(&self, spec: &TrialSpec, core: CoreId) -> DrawKey {
        let mut key = DrawKey {
            words: [
                u64::from(core.get()),
                regime_key(spec.regime),
                condition_index(spec.condition),
                self.regs[core.index()].get().cast_unsigned(),
                spec.index as u64,
                0,
            ],
            len: 5,
        };
        let loaded: CoreSet = spec.cores.iter().copied().collect();
        if !loaded.contains(core) {
            key.words[5] = loaded_mask(loaded);
            key.len = 6;
        }
        key
    }

    fn run(&mut self, spec: &TrialSpec, escape: bool) -> Result<Outcome, Error> {
        let load = load_of(spec);
        let planned = self
            .plan(spec, &load, &self.regs)
            .map_err(|error| Error::Other(Box::new(error)))?;
        let start = self.clock();
        let mut result = TrialResult {
            ran: spec.duration,
            ..TrialResult::default()
        };
        if let Some(first) = load.cores.first() {
            let mut rng = stream(self.seed, Domain::Tctl, self.key(spec, first).words());
            result.tctl_max_c = Some(62 + rng.random_range(0..15));
            if escape {
                result.escaped = CpuId::try_from(first.index() + 2 * self.model.cores())
                    .into_iter()
                    .collect();
            }
        }
        let mut events = Vec::new();
        let mut stalled = None;
        let finished = self.conclude(spec, planned, start, result, &mut events, &mut stalled)?;
        let persisted = self.record_samples(spec, &load, start, stalled);
        Ok(Outcome {
            result: finished,
            events,
            persisted,
        })
    }

    fn conclude(
        &mut self,
        spec: &TrialSpec,
        planned: Option<Failure>,
        start: Duration,
        mut result: TrialResult,
        events: &mut Vec<Event>,
        stalled: &mut Option<CoreId>,
    ) -> Result<Result<TrialResult, TrialResult>, ArithmeticError> {
        let Some(failure) = planned else {
            self.advance(spec.duration)?;
            count(spec, &mut result);
            progress(spec, &result, events);
            return Ok(Ok(result));
        };
        let mut rng = stream(
            self.seed,
            Domain::Signal,
            self.key(spec, failure.core).words(),
        );
        let drawn = self.model.signal_mix(spec.regime).pick(rng.random());
        let signal = failure.signal.unwrap_or(drawn);
        let at = start
            .checked_add(failure.at)
            .ok_or(ArithmeticError::ClockAdvance)?;
        let detail = |what: &str| format!("simulated {what} on core {}", failure.core);
        if failure.then_crash {
            *stalled = Some(failure.core);
            self.set_clock(at)?;
            if signal != Signal::Crash {
                events.push(Event::Signal(failure.core, signal, detail(signal.as_str())));
            }
            if let Some(reset) = failure.reset {
                self.set_next_reset(reset);
            }
            result.ran = failure.at;
            count(spec, &mut result);
            progress(spec, &result, events);
            self.crash();
            return Ok(Err(result));
        }
        Ok(match signal {
            Signal::ComputationError | Signal::Stall | Signal::UnexpectedExit => {
                self.set_clock(at)?;
                result.ran = failure.at;
                result.failure = Some(TrialFailure {
                    signal,
                    core: failure.core,
                    pid: None,
                });
                count(spec, &mut result);
                if signal == Signal::ComputationError {
                    events.push(Event::Signal(
                        failure.core,
                        signal,
                        detail("computation error"),
                    ));
                }
                progress(spec, &result, events);
                Ok(result)
            }
            Signal::CorrectedMce => {
                let pending = self.mce(rng.random(), failure.core, true);
                let boot = self.boot_id().clone();
                if failure.replayed {
                    result.ran = failure.at;
                }
                self.set_clock(
                    start
                        .checked_add(result.ran)
                        .ok_or(ArithmeticError::ClockAdvance)?,
                )?;
                self.log_mce(&boot, pending, at)?;
                count(spec, &mut result);
                progress(spec, &result, events);
                Ok(result)
            }
            Signal::Crash => {
                if !failure.background {
                    *stalled = Some(failure.core);
                }
                self.set_clock(at)?;
                if failure.replayed {
                    self.set_next_reset(ResetKind::Watchdog);
                    events.push(Event::Progress(
                        "simulated replayed crash at recorded exposure".to_owned(),
                    ));
                } else if let Some(reset) = failure.reset {
                    self.set_next_reset(reset);
                } else if !self.next_reset_is_set() {
                    let reset = self.draw_reset(rng.random());
                    self.set_next_reset(reset);
                }
                result.ran = failure.at;
                count(spec, &mut result);
                progress(spec, &result, events);
                if !failure.replayed {
                    self.queue_crash_mce(&mut rng, failure.core, failure.evidence);
                }
                self.crash();
                Err(result)
            }
            Signal::UncorrectedMce => {
                self.set_clock(at)?;
                if failure.replayed {
                    events.push(Event::Signal(
                        failure.core,
                        Signal::UncorrectedMce,
                        "simulated replayed uncorrected machine check".to_owned(),
                    ));
                }
                let pending = self.mce(rng.random(), failure.core, false);
                self.queue_mce(pending);
                self.set_next_reset(ResetKind::SyncFlood);
                self.crash();
                result.ran = failure.at;
                count(spec, &mut result);
                Err(result)
            }
        })
    }

    fn queue_crash_mce(&mut self, rng: &mut StdRng, core: CoreId, evidence: Evidence) {
        match evidence {
            Evidence::Named(named) => self.queue_mce(PendingMce {
                core: named,
                bank: 0,
                bank_type: togi_machine::BankType::LoadStore,
                corrected: false,
            }),
            Evidence::Nothing => {}
            Evidence::Core => {
                if rng.random::<f64>() < self.model.params().crash_mce {
                    let pending = self.mce(rng.random(), core, false);
                    self.queue_mce(pending);
                }
            }
        }
    }

    fn record_samples(
        &mut self,
        spec: &TrialSpec,
        load: &Load<'_>,
        start: Duration,
        stalled: Option<CoreId>,
    ) -> io::Result<()> {
        let snapshot = LastTrial {
            id: Some(spec.id.clone()),
            cores: load.cores,
            threads: spec.workload.workload().threads,
            ran: self.clock().saturating_sub(start),
            stalled,
            voltage: self.model.voltage_state(&self.regs, load).ok().flatten(),
        };
        if let Some(storage) = &self.sample_storage {
            self.samples = LastTrial::default();
            return storage.write(&spec.id, &mut |out| snapshot.encode(out));
        }
        if let Some(dir) = &self.samples_dir {
            self.samples = LastTrial::default();
            snapshot.persist(dir, &spec.id)
        } else {
            self.samples = snapshot;
            Ok(())
        }
    }

    /// Answers a trial from a matching recorded fact, else its script, else the model's draws. `None`
    /// is a pass.
    fn plan(
        &self,
        spec: &TrialSpec,
        load: &Load<'_>,
        profile: &[TargetOffset],
    ) -> Result<Option<Failure>, crate::ShapeError> {
        if let Some(replay) = &self.replay {
            let mut rng = stream(
                self.seed,
                Domain::Replay,
                &[
                    spec.index as u64,
                    regime_key(spec.regime),
                    saturating_key_nanos(spec.duration),
                    text_part(spec.workload.as_str()),
                ],
            );
            if let Some(fact) = replay.answer(&self.bios, load, profile, &mut rng) {
                return Ok(match &fact.outcome {
                    ReplayOutcome::Pass => None,
                    ReplayOutcome::Failure { signal, exposure } => {
                        fact.cores.first().map(|core| Failure {
                            signal: Some(*signal),
                            evidence: Evidence::Nothing,
                            replayed: true,
                            ..Failure::new(core, *exposure)
                        })
                    }
                });
            }
        }
        if let Some(script) = self.scripts.get(&spec.id) {
            return Ok(script.signal.map(|signal| Failure {
                signal: Some(signal),
                reset: script.reset,
                then_crash: script.then_crash,
                ..Failure::new(
                    script.core,
                    Duration::from_secs_f64(script.at_s.min(spec.duration.as_secs_f64())),
                )
            }));
        }
        self.drawn_failure(spec, load, profile)
    }

    /// Draws every source of failure the trial is exposed to and keeps the earliest strictly before the
    /// intended end. Sources come in the fixed order of `visit_sources`, so a tie goes to the earlier one.
    fn drawn_failure(
        &self,
        spec: &TrialSpec,
        load: &Load<'_>,
        profile: &[TargetOffset],
    ) -> Result<Option<Failure>, crate::ShapeError> {
        let duration = spec.duration.as_secs_f64();
        let onset = self.model.params().onset;
        let mut best: Option<(f64, Failure)> = None;
        self.model.visit_sources(profile, load, |source| {
            let (core, mut failure) = self.attribute(&source, spec, load);
            let mut rng = stream(self.seed, source.domain(), self.key(spec, core).words());
            let seconds = onset.failure_time(source.after_s, source.rate, exp1(&mut rng));
            if seconds < duration && best.as_ref().is_none_or(|(known, _)| seconds < *known) {
                failure.at = Duration::from_secs_f64(seconds);
                best = Some((seconds, failure));
            }
        })?;
        Ok(best.map(|(_, failure)| failure))
    }

    /// The core a source's failure is attributed to, and what the failure looks like. The attribution
    /// does not depend on the draw, so only winning sources keep it.
    fn attribute(
        &self,
        source: &HazardSource,
        spec: &TrialSpec,
        load: &Load<'_>,
    ) -> (CoreId, Failure) {
        let unattributed = if self.model.has_regime_signals() {
            None
        } else {
            Some(Signal::Crash)
        };
        let lowest = || load.cores.first();
        let failure = |core: CoreId| Failure::new(core, Duration::ZERO);
        match source.kind {
            SourceKind::Core(core) => (core, failure(core)),
            SourceKind::Idle(core) => (
                core,
                Failure {
                    signal: Some(Signal::Crash),
                    evidence: Evidence::Nothing,
                    ..failure(core)
                },
            ),
            SourceKind::Voltage(core) => (
                core,
                Failure {
                    signal: Some(self.voltage_signal(spec, core)),
                    evidence: Evidence::Nothing,
                    ..failure(core)
                },
            ),
            SourceKind::Ccd(ccd) => {
                let core = load
                    .cores
                    .iter()
                    .find(|&core| self.model.ccd_of(core) == usize::from(ccd))
                    .or_else(lowest)
                    .unwrap_or(CoreId::FIRST);
                (
                    core,
                    Failure {
                        signal: unattributed,
                        evidence: Evidence::Nothing,
                        ..failure(core)
                    },
                )
            }
            SourceKind::Joint(index) => {
                let joint = &self.model.joints()[index];
                let member = joint
                    .members
                    .iter()
                    .map(|member| member.core)
                    .find(|&core| load.cores.contains(core));
                let core = member.unwrap_or(joint.members[0].core);
                (
                    core,
                    Failure {
                        signal: if member.is_none() {
                            Some(Signal::Crash)
                        } else {
                            joint.signal.or(unattributed)
                        },
                        evidence: joint
                            .crash_mce_core
                            .map_or(Evidence::Nothing, Evidence::Named),
                        ..failure(core)
                    },
                )
            }
            SourceKind::Background => {
                let core = lowest().unwrap_or(CoreId::FIRST);
                (
                    core,
                    Failure {
                        signal: Some(Signal::Crash),
                        evidence: Evidence::Nothing,
                        background: true,
                        ..failure(core)
                    },
                )
            }
        }
    }

    /// A rail failure is a computation error with the weight of that signal among the core's mix, else a
    /// crash: nothing else is attributed to a core by the rail.
    fn voltage_signal(&self, spec: &TrialSpec, core: CoreId) -> Signal {
        let mix = self
            .model
            .voltage()
            .and_then(|voltage| {
                voltage.workload(spec.workload.as_str()).cores[core.index()]
                    .signals
                    .as_ref()
            })
            .unwrap_or_else(|| self.model.signal_mix(spec.regime));
        let mut rng = stream(
            self.seed,
            Domain::VoltageSignal,
            self.key(spec, core).words(),
        );
        if rng.random::<f64>() < mix.probability(Signal::ComputationError) {
            Signal::ComputationError
        } else {
            Signal::Crash
        }
    }
}

fn loaded_mask(loaded: CoreSet) -> u64 {
    loaded.iter().fold(0, |mask, core| mask | 1 << core.get())
}

/// Trial names that do not parse as a u32, including larger numbers, get synthetic PID 1000.
/// Parsed names add that reserved base and saturate at the recorded PID field's maximum.
fn saturating_trial_pid(id: &str) -> u32 {
    1000_u32.saturating_add(id.parse().unwrap_or(0))
}

/// Synthetic replay keys retain the largest nanosecond word for longer durations.
fn saturating_key_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Synthetic instruction counters saturate at their recorded field's range.
fn saturating_trial_count(count: impl TryInto<u32>) -> u32 {
    count.try_into().unwrap_or(u32::MAX)
}

/// Stops and continues the trial's schedule has sent by `result.ran`; R6 counts its own bursts.
fn count(spec: &TrialSpec, result: &mut TrialResult) {
    if let Some(schedule) = LoadSchedule::for_spec(spec) {
        let toggles = schedule.toggles(result.ran);
        result.stops = toggles.stops;
        result.conts = toggles.conts;
    }
    if spec.regime == Regime::R6 && !result.ran.is_zero() {
        result.stops = saturating_trial_count(spec.cores.len());
        result.conts = 0;
        let mut at = spec.duration / 2;
        while at < result.ran && at < spec.duration {
            result.conts = saturating_trial_count(u64::from(result.conts) + 1);
            let end = at + BURST;
            if end < result.ran && end < spec.duration {
                result.stops = saturating_trial_count(u64::from(result.stops) + 1);
            }
            at += BURST_EVERY;
        }
    }
}

fn progress(spec: &TrialSpec, result: &TrialResult, events: &mut Vec<Event>) {
    if spec.regime != Regime::R6 {
        return;
    }
    if result.ran > spec.duration / 2 {
        events.push(Event::Progress(
            "first half idle, then 100ms bursts every 2s, one core at a time".to_owned(),
        ));
    }
    events.push(Event::Progress(format!(
        "bursts: {} continues, {} stops",
        result.conts, result.stops
    )));
}

#[cfg(test)]
mod numeric_tests {
    use super::{saturating_key_nanos, saturating_trial_count, saturating_trial_pid};
    use std::time::Duration;
    use test_case::test_case;

    #[test_case(Duration::ZERO, 0; "zero")]
    #[test_case(Duration::from_nanos(u64::MAX), u64::MAX; "largest exact key")]
    #[test_case(Duration::from_nanos(u64::MAX) + Duration::from_nanos(1), u64::MAX; "first saturated key")]
    #[test_case(Duration::MAX, u64::MAX; "largest duration")]
    fn replay_key_nanoseconds_saturate(at: Duration, expected: u64) {
        assert_eq!(saturating_key_nanos(at), expected);
    }

    #[test_case(0, 0; "zero")]
    #[test_case(usize::from(u16::MAX), u32::from(u16::MAX); "exact count")]
    #[test_case(usize::MAX, u32::MAX; "largest platform count")]
    fn instruction_counts_saturate(count: usize, expected: u32) {
        assert_eq!(saturating_trial_count(count), expected);
    }

    #[test_case("0", 1000; "zero")]
    #[test_case("named", 1000; "nonnumeric name")]
    #[test_case("4294966295", u32::MAX; "largest exact synthetic pid")]
    #[test_case("4294966296", u32::MAX; "first saturated synthetic pid")]
    #[test_case("4294967295", u32::MAX; "largest numeric trial name")]
    #[test_case("4294967296", 1000; "numeric name outside u32")]
    fn synthetic_pids_saturate_at_the_recorded_field(id: &str, expected: u32) {
        assert_eq!(saturating_trial_pid(id), expected);
    }

    #[test_case(u64::from(u32::MAX), u32::MAX; "last exact count")]
    #[test_case(u64::from(u32::MAX) + 1, u32::MAX; "first overflowing increment")]
    fn instruction_counter_increments_share_the_count_limit(count: u64, expected: u32) {
        assert_eq!(saturating_trial_count(count), expected);
    }
}
