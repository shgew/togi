use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use jiff::Timestamp;
use togi_machine::{
    BiosContext, BootId, CancellationToken, Check, Clock, CoreInfo, CoreRank, Error, Host, Machine,
    Partial, PerCore, Reporter, Running, SampleError, Signal, SmuValidated, Started, Stopped,
    Swept, TargetOffset, TrialConditions, TrialFailure, TrialResult, TrialSpec, Trials,
};
use togi_sim::day_night::{FailureCause, Scenario, TrialClass, TruthState};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

use super::{EndAnswer, RunError, depth};

type Shared = Rc<RefCell<State>>;

#[derive(Clone, Copy)]
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

struct State {
    scenario: Scenario,
    truth: TruthState,
    host: Simulator,
    base: Machine<'static>,
    seed: u64,
    key: u64,
    day: u32,
    transition: Option<u32>,
    lambda: f64,
    elapsed: Duration,
    night_remaining: Option<Duration>,
    night_active: Duration,
    non_cpu_today: bool,
}

pub(super) struct Runtime(Shared);

impl Runtime {
    pub(super) fn new(
        scenario: Scenario,
        seed: u64,
        lambda: f64,
        transition: Option<u32>,
    ) -> Result<Self, RunError> {
        let count = scenario.ccd_count();
        let definition = MachineDefinition {
            cores: i64::try_from(scenario.topology().len())
                .map_err(|_| RunError::Calendar("core count"))?,
            ..MachineDefinition::default()
        };
        let model = Model::new(&definition, seed)?;
        let host = Simulator::new(
            model,
            RunSettings {
                seed,
                ..RunSettings::default()
            },
        )?;
        let base = host.seams();
        Ok(Self(Rc::new(RefCell::new(State {
            scenario,
            truth: TruthState::cold(count)?,
            host,
            base,
            seed,
            key: 0,
            day: 0,
            transition,
            lambda,
            elapsed: Duration::ZERO,
            night_remaining: None,
            night_active: Duration::ZERO,
            non_cpu_today: false,
        }))))
    }

    pub(super) fn ccd_count(&self) -> usize {
        self.0.borrow().scenario.ccd_count()
    }
    pub(super) fn scenario(&self) -> std::cell::Ref<'_, Scenario> {
        std::cell::Ref::map(self.0.borrow(), |state| &state.scenario)
    }
    pub(super) fn now(&self) -> Duration {
        self.0.borrow().elapsed
    }
    pub(super) fn set_day(&mut self, day: u32) {
        let mut state = self.0.borrow_mut();
        state.day = day;
        state.non_cpu_today = false;
    }
    pub(super) fn cap_epoch(&self) -> u32 {
        let state = self.0.borrow();
        state.scenario.cap_epoch(state.truth_day())
    }
    pub(super) fn safe_depths(
        &self,
        active: Duration,
        fractions: &[f64],
    ) -> Result<Vec<u8>, RunError> {
        let state = self.0.borrow();
        state
            .scenario
            .safe_profile_for(state.truth_day(), state.lambda, active, fractions)?
            .into_iter()
            .map(depth)
            .collect()
    }
    pub(super) fn depths(&self) -> Result<[u8; 2], RunError> {
        let offsets = self.0.borrow_mut().offsets()?;
        Ok([depth(offsets[0])?, depth(offsets[1])?])
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
        Ok(())
    }
    pub(super) fn seams(&self) -> Machine<'static> {
        let mut machine = self.0.borrow().host.seams();
        // The existing simulator splits cores in halves; the opt-in model also supports
        // one physical eight-core CCD, so topology belongs to this adapter.
        machine.host = Box::new(CycleHost {
            inner: machine.host,
            shared: self.0.clone(),
        });
        machine.clock = Box::new(CycleClock(self.0.clone()));
        machine.trials = Box::new(CycleTrials(self.0.clone()));
        machine
    }
    pub(super) fn night_specs(&self) -> Result<Vec<TrialSpec>, RunError> {
        let state = self.0.borrow();
        state
            .scenario
            .night_classes()
            .iter()
            .enumerate()
            .map(|(index, class)| {
                Ok(class.to_spec(
                    format!("night-{}-{index}", state.day),
                    PerCore::default(),
                    index,
                    state.seed,
                )?)
            })
            .collect()
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
        self.0.borrow_mut().advance(None, duration)
    }
    pub(super) fn active(
        &mut self,
        class: &TrialClass,
        duration: Duration,
    ) -> Result<ActiveEvent, RunError> {
        let mut state = self.0.borrow_mut();
        let failure = state.draw(class, duration)?;
        // The registered non-CPU cases contain both graphics and power interruptions. Neither
        // their true subsystem nor the scenario identifier crosses the policy interface.
        let non_cpu = !state.non_cpu_today
            && matches!(state.day, 30 | 120 | 240 | 400 | 600)
            && matches!(
                state.scenario.id(),
                "non-cpu-correct" | "non-cpu-wrong" | "non-cpu-missing"
            );
        let non_cpu_at = duration / 2;
        let event = if non_cpu
            && failure
                .as_ref()
                .is_none_or(|failure| non_cpu_at < failure.elapsed)
        {
            state.non_cpu_today = true;
            let answer = match state.scenario.id() {
                "non-cpu-correct" => Some(EndAnswer::Other),
                "non-cpu-wrong" => Some(EndAnswer::Cpu),
                "non-cpu-missing" => None,
                _ => return Err(RunError::Calendar("non-CPU event scenario")),
            };
            let cause = if matches!(state.day, 30 | 240 | 600) {
                Cause::Graphics
            } else {
                Cause::Power
            };
            ActiveEvent {
                elapsed: non_cpu_at,
                cause: Some(cause),
                answer,
            }
        } else if let Some(failure) = failure {
            ActiveEvent {
                elapsed: failure.elapsed,
                cause: Some(Cause::Truth(failure.cause)),
                answer: Some(EndAnswer::Cpu),
            }
        } else {
            ActiveEvent {
                elapsed: duration,
                cause: None,
                answer: None,
            }
        };
        state.advance(Some(class), event.elapsed)?;
        if event.cause.is_some() {
            state.truth.advance(None, Duration::ZERO)?;
        }
        Ok(event)
    }
}

impl State {
    fn truth_day(&self) -> u32 {
        if let (Some(actual), Some(original)) = (self.transition, self.scenario.transition_day()) {
            if self.day >= actual {
                return original + self.day - actual;
            }
            return self.day.min(original.saturating_sub(1));
        }
        self.day
    }
    fn offsets(&mut self) -> Result<[TargetOffset; 2], RunError> {
        let mut offsets = [TargetOffset::ZERO; 2];
        for info in self.scenario.topology() {
            let value = TargetOffset::new(self.base.smu.offset(info.core)?.0)
                .map_err(|_| RunError::Calendar("SMU readback offset"))?;
            if value.get() < offsets[info.ccd.index()].get() {
                offsets[info.ccd.index()] = value;
            }
        }
        Ok(offsets)
    }
    fn draw(
        &mut self,
        class: &TrialClass,
        duration: Duration,
    ) -> Result<Option<togi_sim::day_night::Failure>, RunError> {
        let offsets = self.offsets()?;
        let result = self.scenario.draw(
            class,
            &offsets[..self.scenario.ccd_count()],
            &self.truth,
            duration,
            self.truth_day(),
            self.seed,
            self.key,
        )?;
        self.key = self
            .key
            .checked_add(1)
            .ok_or(RunError::Calendar("random draw counter"))?;
        Ok(result)
    }
    fn advance(&mut self, class: Option<&TrialClass>, duration: Duration) -> Result<(), RunError> {
        if let Some(remaining) = &mut self.night_remaining {
            if duration > *remaining {
                return Err(RunError::NightOverrun);
            }
            *remaining -= duration;
            if class.is_some() {
                self.night_active += duration;
            }
        }
        self.truth.advance(class, duration)?;
        self.base.clock.sleep(&CancellationToken::new(), duration)?;
        self.elapsed = self
            .elapsed
            .checked_add(duration)
            .ok_or(RunError::Calendar("elapsed duration"))?;
        Ok(())
    }
}

struct CycleHost {
    inner: Box<dyn Host>,
    shared: Shared,
}

impl Host for CycleHost {
    fn boot_id(&mut self) -> Result<BootId, Error> {
        self.inner.boot_id()
    }
    fn topology(&mut self) -> Result<Vec<CoreInfo>, Error> {
        Ok(self.shared.borrow().scenario.topology().to_vec())
    }
    fn validate_smu(&mut self) -> Result<SmuValidated, Error> {
        self.inner.validate_smu()
    }
    fn bios_context(&mut self) -> Result<BiosContext, Error> {
        self.inner.bios_context()
    }
    fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        self.inner.ranking()
    }
    fn preflight(&mut self) -> Vec<Check> {
        self.inner.preflight()
    }
    fn watchdog(&mut self) -> Check {
        self.inner.watchdog()
    }
}

struct CycleClock(Shared);
impl Clock for CycleClock {
    fn now(&self) -> Timestamp {
        self.0.borrow().host.now()
    }
    fn monotonic(&self) -> Result<Duration, Error> {
        Ok(self.0.borrow().host.monotonic())
    }
    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.0
            .borrow_mut()
            .advance(None, duration)
            .map_err(machine_error)
    }
}

struct CycleTrials(Shared);
impl Trials for CycleTrials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(Swept {
            detail: "no external workloads in the calendar simulator".into(),
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
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let class =
                TrialClass::from_spec(&spec).map_err(|error| machine_error(error.into()))?;
            let state = self.0.borrow();
            spec.check_roles(state.scenario.topology())
                .map_err(|error| machine_error(RunError::Roles(error)))?;
            let remaining = state.night_remaining.ok_or_else(|| {
                machine_error(RunError::Calendar("trials require a night allocation"))
            })?;
            if spec.duration > remaining {
                return Err(machine_error(RunError::NightOverrun));
            }
            Ok(Box::new(CycleRunning {
                shared: self.0.clone(),
                spec,
                class,
                waited: false,
            }) as Box<dyn Running>)
        };
        build().map_err(|error| Partial {
            value: Some(confirmed()),
            error,
        })
    }
    fn ended(&mut self, _id: &str) -> Result<(), Error> {
        Ok(())
    }
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
        Started {
            cpus: self.spec.cpus.clone(),
            ..Started::default()
        }
    }
    fn wait(
        &mut self,
        cancel: &CancellationToken,
        reporter: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        let mut execute = || {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if self.waited {
                return Err(machine_error(RunError::Calendar("trial waited twice")));
            }
            self.waited = true;
            let mut state = self.shared.borrow_mut();
            let failure = state
                .draw(&self.class, self.spec.duration)
                .map_err(machine_error)?;
            let ran = failure
                .as_ref()
                .map_or(self.spec.duration, |failure| failure.elapsed);
            state
                .advance(Some(&self.class), ran)
                .map_err(machine_error)?;
            if failure.is_some() {
                state
                    .truth
                    .advance(None, Duration::ZERO)
                    .map_err(|error| machine_error(error.into()))?;
            }
            let failure = if let Some(failure) = failure {
                let ccd = match failure.cause {
                    FailureCause::Ccd(ccd) => Some(ccd),
                    FailureCause::Interaction | FailureCause::Background => None,
                };
                let core = self
                    .spec
                    .cores
                    .iter()
                    .copied()
                    .find(|core| {
                        ccd.is_none_or(|ccd| {
                            state
                                .scenario
                                .topology()
                                .iter()
                                .any(|info| info.core == *core && info.ccd == ccd)
                        })
                    })
                    .ok_or_else(|| {
                        machine_error(RunError::Calendar("failure outside the trial's loaded set"))
                    })?;
                reporter.signal(
                    core,
                    Signal::ComputationError,
                    "night trial observed a failure",
                );
                Some(TrialFailure {
                    core,
                    signal: Signal::ComputationError,
                })
            } else {
                None
            };
            Ok(TrialResult {
                ran,
                failure,
                ..TrialResult::default()
            })
        };
        execute().map_err(|error| Partial {
            value: TrialResult::default(),
            error,
        })
    }
    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        Ok(confirmed())
    }
}

fn machine_error(error: RunError) -> Error {
    Error::Other(Box::new(error))
}

#[expect(
    clippy::disallowed_methods,
    reason = "the simulator is a seam: no workload ever runs, so teardown is always confirmed"
)]
fn confirmed() -> Stopped {
    Stopped::confirmed()
}
