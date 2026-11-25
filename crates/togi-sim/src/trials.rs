//! Simulated trials: typed outcomes applied to virtual clocks, resets and kernel evidence.

use rand::RngExt;
use std::io;
use std::time::Duration;
use togi_machine::{
    CancellationToken, Condition, ContentId, CoreId, CoreSet, CpuId, Error, FrequencyJournal,
    LoadSchedule, Partial, Regime, Reporter, ResetKind, Running, Signal, Started, Stopped, Swept,
    TrialConditions, TrialFailure, TrialResult, TrialSpec, Trials, WorkloadId,
};

use crate::random::{Domain, regime_key, stream};
use crate::runtime::{ArithmeticError, PendingMce, Runtime, Shared};
use crate::samples::LastTrial;
use crate::source::TrialOutcome;

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

/// The controller pid a simulated trial reports: fixed, so the same seed journals the same
/// `trial.start`, and below the 1000 every simulated workload pid starts from.
pub const CONTROLLER_PID: u32 = 100;

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

    /// The simulated machine writes no cpufreq limit, so it has no frequency state to recover.
    fn recover_frequency(
        &mut self,
        _stopped: &Stopped,
        _journal: &mut dyn FrequencyJournal,
    ) -> Result<String, Error> {
        Ok(String::new())
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
        _journal: &mut dyn FrequencyJournal,
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

    fn controller_pid(&self) -> u32 {
        CONTROLLER_PID
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

    fn stop(self: Box<Self>, _journal: &mut dyn FrequencyJournal) -> Result<Stopped, Error> {
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

struct TimedEffect {
    at: Duration,
    effect: Effect,
}

enum Effect {
    Backend {
        core: CoreId,
        signal: Signal,
    },
    Corrected {
        mce: PendingMce,
    },
    Crash {
        reset: Option<ResetKind>,
        stalled: Option<CoreId>,
        mce: Option<PendingMce>,
        report: CrashReport,
    },
}

enum CrashReport {
    Progress,
    Signal(CoreId, Signal),
    Uncorrected(CoreId),
}

impl CrashReport {
    fn report(self, spec: &TrialSpec, result: &TrialResult, events: &mut Vec<Event>) {
        match self {
            Self::Progress => {}
            Self::Signal(core, signal) => events.push(Event::Signal(
                core,
                signal,
                format!("simulated {} on core {core}", signal.as_str()),
            )),
            Self::Uncorrected(core) => {
                events.push(Event::Signal(
                    core,
                    Signal::UncorrectedMce,
                    "scripted uncorrected machine check".to_owned(),
                ));
                return;
            }
        }
        progress(spec, result, events);
    }
}

const fn load_store_mce(core: CoreId) -> PendingMce {
    PendingMce {
        core,
        bank: 0,
        bank_type: togi_machine::BankType::LoadStore,
        corrected: false,
    }
}

const fn condition_index(condition: Condition) -> u64 {
    match condition {
        Condition::Alone => 0,
        Condition::Together => 1,
        Condition::Parked => 2,
    }
}

impl Runtime {
    fn run(&mut self, spec: &TrialSpec, escape: bool) -> Result<Outcome, Error> {
        let loaded: CoreSet = spec.cores.iter().copied().collect();
        let effect = self.resolve_scripted(spec, (self.source.outcome)(spec, &self.regs))?;
        let start = self.clock();
        let mut result = TrialResult {
            ran: spec.duration,
            ..TrialResult::default()
        };
        // Use the lowest loaded core, independent of placement order. The Tctl stream takes
        // these five words, in this order, even when the trial ends in a reset.
        if let Some(first) = loaded.first() {
            let key = [
                u64::from(first.get()),
                regime_key(spec.regime),
                condition_index(spec.condition),
                self.regs[first.index()].get().cast_unsigned(),
                spec.index as u64,
            ];
            let mut rng = stream(self.seed, Domain::Tctl, &key);
            result.tctl_max_c = Some(62 + rng.random_range(0..15));
            if escape {
                result.escaped = CpuId::try_from(first.index() + 2 * self.regs.len())
                    .into_iter()
                    .collect();
            }
        }
        let mut events = Vec::new();
        let mut stalled = None;
        let finished = self.conclude(spec, effect, start, result, &mut events, &mut stalled)?;
        let persisted = self.record_samples(spec, loaded, start, stalled);
        Ok(Outcome {
            result: finished,
            events,
            persisted,
        })
    }

    fn conclude(
        &mut self,
        spec: &TrialSpec,
        effect: Option<TimedEffect>,
        start: Duration,
        mut result: TrialResult,
        events: &mut Vec<Event>,
        stalled: &mut Option<CoreId>,
    ) -> Result<Result<TrialResult, TrialResult>, ArithmeticError> {
        let Some(failure) = effect else {
            self.advance(spec.duration)?;
            count(spec, &mut result);
            progress(spec, &result, events);
            return Ok(Ok(result));
        };
        let at = start
            .checked_add(failure.at)
            .ok_or(ArithmeticError::ClockAdvance)?;
        Ok(match failure.effect {
            Effect::Backend { core, signal } => {
                self.set_clock(at)?;
                result.ran = failure.at;
                result.failure = Some(TrialFailure {
                    signal,
                    core,
                    pid: None,
                });
                count(spec, &mut result);
                if signal == Signal::ComputationError {
                    events.push(Event::Signal(
                        core,
                        signal,
                        format!("simulated computation error on core {core}"),
                    ));
                }
                progress(spec, &result, events);
                Ok(result)
            }
            Effect::Corrected { mce } => {
                let boot = self.boot_id().clone();
                self.set_clock(
                    start
                        .checked_add(spec.duration)
                        .ok_or(ArithmeticError::ClockAdvance)?,
                )?;
                self.log_mce(&boot, mce, at)?;
                count(spec, &mut result);
                progress(spec, &result, events);
                Ok(result)
            }
            Effect::Crash {
                reset,
                stalled: stopped_core,
                mce,
                report,
            } => {
                *stalled = stopped_core;
                self.set_clock(at)?;
                if let Some(reset) = reset {
                    self.set_next_reset(reset);
                }
                result.ran = failure.at;
                count(spec, &mut result);
                report.report(spec, &result, events);
                if let Some(mce) = mce {
                    self.queue_mce(mce);
                }
                self.crash();
                Err(result)
            }
        })
    }

    fn resolve_scripted(
        &self,
        spec: &TrialSpec,
        outcome: TrialOutcome,
    ) -> Result<Option<TimedEffect>, Error> {
        let (at, core, effect) = match outcome {
            TrialOutcome::Pass => return Ok(None),
            TrialOutcome::ComputationError { core, at } => (
                at,
                Some(core),
                Effect::Backend {
                    core,
                    signal: Signal::ComputationError,
                },
            ),
            TrialOutcome::UnexpectedExit { core, at } => (
                at,
                Some(core),
                Effect::Backend {
                    core,
                    signal: Signal::UnexpectedExit,
                },
            ),
            TrialOutcome::Stall { core, at } => (
                at,
                Some(core),
                Effect::Backend {
                    core,
                    signal: Signal::Stall,
                },
            ),
            TrialOutcome::ReportedFailureThenReset { core, failure, at } => (
                at,
                Some(core),
                Effect::Crash {
                    reset: None,
                    stalled: Some(core),
                    mce: None,
                    report: CrashReport::Signal(core, failure.signal()),
                },
            ),
            TrialOutcome::CorrectedMce { mce, at } => (
                at,
                Some(mce.core),
                Effect::Corrected {
                    mce: PendingMce {
                        core: mce.core,
                        bank: mce.bank,
                        bank_type: mce.bank_type,
                        corrected: true,
                    },
                },
            ),
            TrialOutcome::Reset { stalled, kind, at } => (
                at,
                stalled,
                Effect::Crash {
                    reset: Some(kind),
                    stalled,
                    mce: None,
                    report: CrashReport::Progress,
                },
            ),
            TrialOutcome::ResetWithMce {
                mce,
                stalled,
                kind,
                at,
            } => {
                if let Some(core) = stalled {
                    self.existing_core(core)?;
                }
                (
                    at,
                    Some(mce.core),
                    Effect::Crash {
                        reset: Some(kind),
                        stalled,
                        mce: Some(PendingMce {
                            core: mce.core,
                            bank: mce.bank,
                            bank_type: mce.bank_type,
                            corrected: false,
                        }),
                        report: CrashReport::Progress,
                    },
                )
            }
            TrialOutcome::UncorrectedMce { core, at } => (
                at,
                Some(core),
                Effect::Crash {
                    reset: Some(ResetKind::SyncFlood),
                    stalled: None,
                    mce: Some(load_store_mce(core)),
                    report: CrashReport::Uncorrected(core),
                },
            ),
        };
        if let Some(core) = core {
            self.existing_core(core)?;
        }
        if at > spec.duration {
            return Err(other(format!(
                "scripted outcome of trial {} falls after its end",
                spec.id
            )));
        }
        Ok(Some(TimedEffect { at, effect }))
    }

    fn record_samples(
        &mut self,
        spec: &TrialSpec,
        loaded: CoreSet,
        start: Duration,
        stalled: Option<CoreId>,
    ) -> io::Result<()> {
        let snapshot = LastTrial {
            id: Some(spec.id.clone()),
            cores: loaded,
            threads: spec.workload.workload().threads,
            ran: self.clock().saturating_sub(start),
            stalled,
            voltage: self.source.voltage_state(),
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
}

/// Trial names that do not parse as a u32, including larger numbers, get synthetic PID 1000.
/// Parsed names add that reserved base and saturate at the recorded PID field's maximum.
fn saturating_trial_pid(id: &str) -> u32 {
    1000_u32.saturating_add(id.parse().unwrap_or(0))
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
    use super::{saturating_trial_count, saturating_trial_pid};
    use test_case::test_case;

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
