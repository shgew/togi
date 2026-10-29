//! Exact physical-core machine seams and actual-history accounting for successor runs.
//!
//! The SMU adapter records full readbacks at every changed profile, including zero-time
//! changes between trials. Only this runner-side module retains truth causes and integrals;
//! policies see ordinary machine results, never the observation ledger.

use std::cell::{Ref, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use togi_machine::{
    BiosContext, BootId, CancellationToken, Check, Clock, CoreId, CoreInfo, CoreRank, Error,
    Host, Machine, ObservedOffset, Partial, PerCore, Reporter, Running, SampleError, Signal,
    Smu, SmuValidated, Started, Stopped, Swept, TargetOffset, TrialConditions, TrialFailure,
    TrialResult, TrialSpec, Trials,
};
use togi_sim::day_night::successor::{
    Depth, Exposure, Failure, FailureCause, Profile, Scenario, SliceSeconds, TruthState,
};
use togi_sim::day_night::{NonCpuAnswer, NonCpuCause, TrialClass};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

use super::super::{EndAnswer, RunError, depth};

type Shared = Rc<RefCell<State>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Cause {
    Truth(FailureCause),
    Graphics,
    Power,
}

pub(super) struct ActiveEvent {
    pub elapsed: Duration,
    pub cause: Option<Cause>,
    pub answer: Option<EndAnswer>,
}

/// One exact-profile segment, or a zero-time full-profile readback boundary.
pub(super) struct Observation {
    pub before: Duration,
    pub after: Duration,
    pub profile: PerCore<u8>,
    pub night: bool,
    pub kind: ObservationKind,
}

pub(super) enum ObservationKind {
    Active {
        class: TrialClass,
        seconds: SliceSeconds,
        /// Full requested run duration, distinct from the exact class's nominal duration.
        /// Count once when `launch` is true, including a run interrupted before completion.
        intended: Duration,
        exposure: Exposure,
        cause: Option<Cause>,
        trial_id: Option<String>,
        /// True only on the first slice of a run: intended seconds count once per launch.
        launch: bool,
    },
    Sleep,
    /// `Observation::profile` is the new, completely read-back profile.
    ProfileChange,
}

struct State {
    scenario: Scenario,
    truth: TruthState,
    host: Simulator,
    base: Machine<'static>,
    profile: Profile,
    seed: u64,
    key: u64,
    day: u32,
    elapsed: Duration,
    night_remaining: Option<Duration>,
    night_active: Duration,
    non_cpu_today: bool,
    deadline: Option<Instant>,
    observations: Vec<Observation>,
}

pub(super) struct Runtime(Shared);

impl Runtime {
    pub(super) fn new(scenario: Scenario, seed: u64) -> Result<Self, RunError> {
        let definition = MachineDefinition {
            cores: i64::try_from(scenario.topology().len())
                .map_err(|_| RunError::Calendar("core count"))?,
            ..MachineDefinition::default()
        };
        let model = Model::new(&definition, seed)?;
        let host = Simulator::new(model, RunSettings { seed, ..RunSettings::default() })?;
        let base = host.seams();
        let truth = TruthState::cold(scenario.topology())?;
        let profile = Profile::uniform(scenario.topology(), Depth::default());
        Ok(Self(Rc::new(RefCell::new(State {
            scenario,
            truth,
            host,
            base,
            profile,
            seed,
            key: 0,
            day: 1,
            elapsed: Duration::ZERO,
            night_remaining: None,
            night_active: Duration::ZERO,
            non_cpu_today: false,
            deadline: None,
            observations: Vec::new(),
        }))))
    }

    pub(super) fn scenario(&self) -> Ref<'_, Scenario> {
        Ref::map(self.0.borrow(), |state| &state.scenario)
    }

    pub(super) fn now(&self) -> Duration {
        self.0.borrow().elapsed
    }

    pub(super) fn set_day(&mut self, day: u32) {
        let mut state = self.0.borrow_mut();
        state.day = day;
        state.non_cpu_today = false;
    }

    pub(super) fn set_deadline(&mut self, deadline: Option<Instant>) {
        self.0.borrow_mut().deadline = deadline;
    }

    pub(super) fn depths(&self) -> Result<PerCore<u8>, RunError> {
        Ok(profile_depths(&self.0.borrow_mut().read_profile()?))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the synthetic firmware reset restores simulated registers, never hardware"
    )]
    pub(super) fn firmware_changed(&mut self) -> Result<(), RunError> {
        let mut state = self.0.borrow_mut();
        let mut context = state.base.host.bios_context()?;
        context.bios_version.push_str("-transition");
        state.host.set_bios_context(context);
        state.base.smu.set_all_offsets(TargetOffset::ZERO)?;
        state.capture_profile()?;
        // A firmware reset is an interruption even when the previous profile was stock.
        state.truth.interrupt();
        Ok(())
    }

    pub(super) fn seams(&self) -> Machine<'static> {
        let mut machine = self.0.borrow().host.seams();
        machine.host = Box::new(CycleHost { inner: machine.host, shared: self.0.clone() });
        machine.smu = Box::new(CycleSmu(self.0.clone()));
        machine.clock = Box::new(CycleClock(self.0.clone()));
        machine.trials = Box::new(CycleTrials(self.0.clone()));
        machine
    }

    pub(super) fn night_specs(&self) -> Result<Vec<TrialSpec>, RunError> {
        let state = self.0.borrow();
        state.scenario.night_classes().iter().enumerate().map(|(index, class)| {
            Ok(class.to_spec(
                format!("night-{}-{index}", state.day),
                PerCore::default(),
                index,
                state.seed,
            )?)
        }).collect()
    }

    pub(super) fn begin_night(&mut self, budget: Duration) {
        let mut state = self.0.borrow_mut();
        state.night_remaining = Some(budget);
        state.night_active = Duration::ZERO;
    }

    pub(super) fn end_night(&mut self) -> Duration {
        let mut state = self.0.borrow_mut();
        state.night_remaining = None;
        state.night_active
    }

    pub(super) fn sleep(&mut self, duration: Duration) -> Result<(), RunError> {
        self.0.borrow_mut().sleep(duration)
    }

    pub(super) fn drain_observations(&mut self) -> Vec<Observation> {
        std::mem::take(&mut self.0.borrow_mut().observations)
    }

    pub(super) fn active(
        &mut self,
        class: &TrialClass,
        duration: Duration,
    ) -> Result<ActiveEvent, RunError> {
        let mut state = self.0.borrow_mut();
        state.check_deadline()?;
        state.capture_profile()?;
        let failure = state.draw(class, duration)?;
        let non_cpu = state.scenario.non_cpu_event(state.day)?
            .filter(|_| !state.non_cpu_today);
        let non_cpu_at = duration / 2;
        let event = if let Some((cause, answer)) = non_cpu
            && failure.as_ref().is_none_or(|failure| non_cpu_at < failure.elapsed)
        {
            state.non_cpu_today = true;
            ActiveEvent {
                elapsed: non_cpu_at,
                cause: Some(match cause {
                    NonCpuCause::Graphics => Cause::Graphics,
                    NonCpuCause::Power => Cause::Power,
                }),
                answer: match answer {
                    NonCpuAnswer::NotCpu => Some(EndAnswer::Other),
                    NonCpuAnswer::Cpu => Some(EndAnswer::Cpu),
                    NonCpuAnswer::Missing => None,
                },
            }
        } else if let Some(failure) = failure {
            ActiveEvent {
                elapsed: failure.elapsed,
                cause: Some(Cause::Truth(failure.cause)),
                answer: Some(EndAnswer::Cpu),
            }
        } else {
            ActiveEvent { elapsed: duration, cause: None, answer: None }
        };
        state.active(class, event.elapsed, duration, event.cause, None)?;
        if event.cause.is_some() {
            state.truth.interrupt();
        }
        Ok(event)
    }
}

impl State {
    fn check_deadline(&self) -> Result<(), RunError> {
        if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            Err(RunError::BudgetExpired)
        } else {
            Ok(())
        }
    }

    fn read_profile(&mut self) -> Result<Profile, RunError> {
        let mut depths = PerCore::default();
        for info in self.scenario.topology() {
            let offset = TargetOffset::new(self.base.smu.offset(info.core)?.0)
                .map_err(|_| RunError::Calendar("SMU readback offset"))?;
            depths.set(info.core, Depth::new(depth(offset)?)?);
        }
        Ok(Profile::new(depths, self.scenario.topology())?)
    }

    fn capture_profile(&mut self) -> Result<(), RunError> {
        let profile = self.read_profile()?;
        if profile != self.profile {
            self.truth.interrupt();
            self.observations.push(Observation {
                before: self.elapsed,
                after: self.elapsed,
                profile: profile_depths(&profile),
                night: self.night_remaining.is_some(),
                kind: ObservationKind::ProfileChange,
            });
            self.profile = profile;
        }
        Ok(())
    }

    fn draw(&mut self, class: &TrialClass, duration: Duration) -> Result<Option<Failure>, RunError> {
        let result = self.scenario.draw(
            class, &self.profile, &self.truth, duration, self.day, self.seed, self.key,
        )?;
        self.key = self.key.checked_add(1)
            .ok_or(RunError::Calendar("random draw counter"))?;
        Ok(result)
    }

    fn advance_clock(&mut self, class: Option<&TrialClass>, duration: Duration) -> Result<Duration, RunError> {
        let after = self.elapsed.checked_add(duration)
            .ok_or(RunError::Calendar("elapsed duration"))?;
        if self.night_remaining.is_some_and(|remaining| duration > remaining) {
            return Err(RunError::NightOverrun);
        }
        let night_active = if class.is_some() && self.night_remaining.is_some() {
            self.night_active.checked_add(duration)
                .ok_or(RunError::Calendar("night active duration"))?
        } else {
            self.night_active
        };
        self.truth.advance_profile(class, &self.profile, duration)?;
        self.base.clock.sleep(&CancellationToken::new(), duration)?;
        if let Some(remaining) = &mut self.night_remaining {
            *remaining -= duration;
            self.night_active = night_active;
        }
        let before = self.elapsed;
        self.elapsed = after;
        Ok(before)
    }

    fn sleep(&mut self, duration: Duration) -> Result<(), RunError> {
        self.check_deadline()?;
        self.capture_profile()?;
        let before = self.advance_clock(None, duration)?;
        self.observations.push(Observation {
            before,
            after: self.elapsed,
            profile: profile_depths(&self.profile),
            night: self.night_remaining.is_some(),
            kind: ObservationKind::Sleep,
        });
        Ok(())
    }

    fn active(
        &mut self,
        class: &TrialClass,
        duration: Duration,
        intended: Duration,
        cause: Option<Cause>,
        trial_id: Option<&str>,
    ) -> Result<(), RunError> {
        let mut remaining = duration;
        let mut launch = true;
        // Daily use may continue one exact class beyond its nominal trial length. Checkpoints
        // keep slice_seconds within its prefix contract without restarting the truth clock.
        loop {
            self.check_deadline()?;
            let ran = remaining.min(class.duration());
            let seconds = self.scenario.slice_seconds(
                class,
                &self.profile,
                &self.truth,
                ran,
                Duration::from_secs(u64::from(class.regime().warm_up_s())),
            )?;
            let exposure = self.scenario.evaluate(
                class, &self.profile, &self.truth, ran, self.day,
            )?;
            let before = self.advance_clock(Some(class), ran)?;
            remaining -= ran;
            self.observations.push(Observation {
                before,
                after: self.elapsed,
                profile: profile_depths(&self.profile),
                night: self.night_remaining.is_some(),
                kind: ObservationKind::Active {
                    class: class.clone(),
                    seconds,
                    intended,
                    exposure,
                    cause: if remaining.is_zero() { cause } else { None },
                    trial_id: trial_id.map(str::to_owned),
                    launch,
                },
            });
            if remaining.is_zero() {
                return Ok(());
            }
            launch = false;
        }
    }
}

fn profile_depths(profile: &Profile) -> PerCore<u8> {
    profile.depths().iter().map(|(core, depth)| (core, depth.get())).collect()
}

struct CycleSmu(Shared);

impl Smu for CycleSmu {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error> {
        self.0.borrow_mut().base.smu.offset(core)
    }

    #[expect(clippy::disallowed_methods, reason = "this adapter writes simulated registers only")]
    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error> {
        let mut state = self.0.borrow_mut();
        state.base.smu.set_offset(core, offset)?;
        state.capture_profile().map_err(machine_error)
    }

    #[expect(clippy::disallowed_methods, reason = "this adapter writes simulated registers only")]
    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
        let mut state = self.0.borrow_mut();
        state.base.smu.set_all_offsets(offset)?;
        state.capture_profile().map_err(machine_error)
    }

    fn emergency_zero(&mut self, validated: SmuValidated, stopped: &Stopped) -> Result<(), Error> {
        let mut state = self.0.borrow_mut();
        state.base.smu.emergency_zero(validated, stopped)?;
        state.capture_profile().map_err(machine_error)
    }
}

struct CycleHost {
    inner: Box<dyn Host>,
    shared: Shared,
}

impl Host for CycleHost {
    fn boot_id(&mut self) -> Result<BootId, Error> { self.inner.boot_id() }
    fn topology(&mut self) -> Result<Vec<CoreInfo>, Error> {
        Ok(self.shared.borrow().scenario.topology().to_vec())
    }
    fn validate_smu(&mut self) -> Result<SmuValidated, Error> { self.inner.validate_smu() }
    fn bios_context(&mut self) -> Result<BiosContext, Error> { self.inner.bios_context() }
    fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> { self.inner.ranking() }
    fn preflight(&mut self) -> Vec<Check> { self.inner.preflight() }
    fn backends(&mut self) -> Vec<togi_machine::BackendCheck> { self.inner.backends() }
    fn watchdog(&mut self) -> Check { self.inner.watchdog() }
}

struct CycleClock(Shared);
impl Clock for CycleClock {
    fn now(&self) -> Timestamp { self.0.borrow().host.now() }
    fn monotonic(&self) -> Result<Duration, Error> { Ok(self.0.borrow().host.monotonic()) }
    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.0.borrow_mut().sleep(duration).map_err(machine_error)
    }
}

struct CycleTrials(Shared);
impl Trials for CycleTrials {
    fn controller_pid(&self) -> u32 { togi_sim::CONTROLLER_PID }
    fn content(&mut self, workload: togi_machine::WorkloadId) -> Result<togi_machine::ContentId, Error> {
        Ok(togi_sim::content_id(workload))
    }
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(Swept {
            detail: "no external workloads in the successor calendar simulator".into(),
            stopped: confirmed(),
        })
    }
    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        _ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let build = || {
            if cancel.is_cancelled() { return Err(Error::Cancelled); }
            let class = TrialClass::from_spec(&spec)
                .map_err(|error| machine_error(error.into()))?;
            let state = self.0.borrow();
            state.check_deadline().map_err(machine_error)?;
            spec.check_roles(state.scenario.topology())
                .map_err(|error| machine_error(RunError::Roles(error)))?;
            let remaining = state.night_remaining.ok_or_else(|| {
                machine_error(RunError::Calendar("trials require a night allocation"))
            })?;
            if remaining.is_zero() { return Err(Error::Cancelled); }
            Ok(Box::new(CycleRunning {
                shared: self.0.clone(), spec, class, waited: false,
            }) as Box<dyn Running>)
        };
        build().map_err(|error| Partial { value: Some(confirmed()), error })
    }
    fn ended(&mut self, _id: &str) -> Result<(), Error> { Ok(()) }
    fn samples<'a>(
        &'a self,
        _id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, SampleError>> + 'a> {
        Box::new(std::iter::empty())
    }
}

struct CycleRunning {
    shared: Shared,
    spec: TrialSpec,
    class: TrialClass,
    waited: bool,
}

impl Running for CycleRunning {
    fn started(&self) -> Started {
        Started { cpus: self.spec.cpus.clone(), ..Started::default() }
    }
    fn wait(
        &mut self,
        cancel: &CancellationToken,
        reporter: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        let mut execute = || -> Result<(TrialResult, bool), Error> {
            if cancel.is_cancelled() { return Err(Error::Cancelled); }
            if self.waited {
                return Err(machine_error(RunError::Calendar("trial waited twice")));
            }
            self.waited = true;
            let mut state = self.shared.borrow_mut();
            state.check_deadline().map_err(machine_error)?;
            state.capture_profile().map_err(machine_error)?;
            let window = self.spec.duration.min(state.night_remaining.ok_or_else(|| {
                machine_error(RunError::Calendar("trials require a night allocation"))
            })?);
            let failure = state.draw(&self.class, window).map_err(machine_error)?;
            let ran = failure.as_ref().map_or(window, |failure| failure.elapsed);
            let operator_end = failure.is_none() && window < self.spec.duration;
            state.active(
                &self.class, ran, self.spec.duration,
                failure.map(|failure| Cause::Truth(failure.cause)),
                Some(&self.spec.id),
            ).map_err(machine_error)?;
            if failure.is_some() || operator_end { state.truth.interrupt(); }
            let failure = if let Some(failure) = failure {
                // Local failures retain their physical core. Joint and platform failures have
                // no policy-visible culprit: report the ordinary first loaded trial core.
                let core = match failure.cause {
                    FailureCause::Core(core) => Some(core),
                    FailureCause::Interaction(_) | FailureCause::Background => self.spec.cores.first().copied(),
                }.filter(|core| self.spec.cores.contains(core)).ok_or_else(|| {
                    machine_error(RunError::Calendar("failure outside the trial's loaded set"))
                })?;
                reporter.signal(core, Signal::ComputationError, None, "night trial observed a failure");
                Some(TrialFailure { core, signal: Signal::ComputationError, pid: None })
            } else { None };
            Ok((TrialResult { ran, failure, ..TrialResult::default() }, operator_end))
        };
        match execute() {
            Ok((result, false)) => Ok(result),
            Ok((result, true)) => Err(Partial { value: result, error: Error::Cancelled }),
            Err(error) => Err(Partial { value: TrialResult::default(), error }),
        }
    }
    fn stop(self: Box<Self>) -> Result<Stopped, Error> { Ok(confirmed()) }
}

fn machine_error(error: RunError) -> Error { Error::Other(Box::new(error)) }

#[expect(
    clippy::disallowed_methods,
    reason = "the simulator is a seam: no workload ever runs, so teardown is always confirmed"
)]
fn confirmed() -> Stopped { Stopped::confirmed() }

#[cfg(test)]
mod tests;
