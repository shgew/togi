//! The simulated machine's handle: seams for the session and controls for tests.

use jiff::{SignedDuration, Timestamp};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use togi_machine::{Backend, BiosContext, CoreId, Machine, ResetKind, Signal};

use crate::adapters::{SimClock, SimHost, SimKernel, SimSmu};
use crate::faults::SmuHook;
use crate::model::{Model, Script, failure_signal};
use crate::persisted::{Persisted, RestoreError};
use crate::replay::Replay;
use crate::runtime::{ArithmeticError, RunSettings, Runtime, Shared};
use crate::trials::SimTrials;

/// A script refused by [`Simulator::script_trial`].
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ScriptError {
    #[error("script trial {trial} core {core} outside the machine's {cores} cores")]
    Core {
        trial: String,
        core: CoreId,
        cores: usize,
    },
    #[error("script trial {trial} at_s {at_s} must be finite and nonnegative")]
    At { trial: String, at_s: f64 },
    #[error("script trial {trial} signal {signal:?} is only supported by replay")]
    Signal { trial: String, signal: Signal },
}

/// A deterministic virtual machine. Cloning the handle shares the machine. [`seams`](Self::seams) hands the session its adapters; every adapter and
/// this handle share one runtime, so a fault armed here shows in the next seam call.
#[derive(Clone)]
pub struct Simulator {
    shared: Shared,
}

impl Simulator {
    /// Starts the machine one reboot after its recorded history.
    ///
    /// # Errors
    ///
    /// The next boot number or resumed timestamp cannot be represented.
    pub fn new(model: Model, settings: RunSettings) -> Result<Self, ArithmeticError> {
        Ok(Self {
            shared: Rc::new(RefCell::new(Runtime::new(model, settings)?)),
        })
    }

    /// The seams of this machine. They may be taken any number of times; all act on the same machine.
    pub fn seams(&self) -> Machine<'static> {
        Machine {
            clock: Box::new(SimClock(self.shared.clone())),
            smu: Box::new(SimSmu(self.shared.clone())),
            host: Box::new(SimHost(self.shared.clone())),
            trials: Box::new(SimTrials(self.shared.clone())),
            kernel: Box::new(SimKernel(self.shared.clone())),
        }
    }

    pub fn model(&self) -> Model {
        self.shared.borrow().model.clone()
    }

    /// The wall clock, which [`jump_wall`](Self::jump_wall) can move.
    pub fn now(&self) -> Timestamp {
        self.shared.borrow().wall()
    }

    /// Time since the current boot.
    pub fn monotonic(&self) -> Duration {
        self.shared.borrow().monotonic()
    }

    /// Moves the wall clock without moving the boot-local monotonic clock.
    ///
    /// # Errors
    ///
    /// The accumulated offset or resulting timestamp cannot be represented. The clock is unchanged.
    pub fn jump_wall(&self, by: SignedDuration) -> Result<(), ArithmeticError> {
        self.shared.borrow_mut().jump_wall(by)
    }

    /// Crashes the machine; its next reset is a watchdog reset unless one is set.
    pub fn crash(&self) {
        self.shared.borrow_mut().crash();
    }

    /// Sets the reason the next boot reports for the reset before it.
    pub fn next_reset(&self, kind: ResetKind) {
        self.shared.borrow_mut().set_next_reset(kind);
    }

    pub fn power_loss(&self) {
        self.next_reset(ResetKind::PowerLoss);
        self.crash();
    }

    pub fn hold_power_button(&self) {
        self.next_reset(ResetKind::PowerButton);
        self.crash();
    }

    /// Boots again after [`crate::REBOOT_TIME`]: a new boot ID, the BIOS offsets restored, and the machine
    /// checks queued by the crash logged at the new boot's monotonic zero.
    ///
    /// # Errors
    ///
    /// The next boot number, elapsed time or wall timestamp cannot be represented. The machine is unchanged.
    pub fn reboot(&self) -> Result<(), ArithmeticError> {
        self.shared.borrow_mut().reboot()
    }

    /// Explanations of every trial that ran alone while another core had a nonzero offset.
    pub fn violations(&self) -> Vec<String> {
        self.shared.borrow().violations.clone()
    }

    /// Fails the `nth` accepted SMU write from now (1 is the next one); the register is unchanged.
    pub fn fail_write(&self, nth: u32) {
        self.shared.borrow_mut().faults.fail_write_in = nth;
    }

    /// Crashes the machine at the `nth` accepted SMU write from now, before it takes effect.
    pub fn crash_at_write(&self, nth: u32) {
        self.shared.borrow_mut().faults.crash_write_in = nth;
    }

    /// Crashes the machine at the first SMU write of each of the next `boots` boots, this one included
    /// if it has not written yet.
    pub fn crash_before_apply(&self, boots: u32) {
        self.shared.borrow_mut().faults.crash_before_apply = boots;
    }

    /// Makes the first readback of `core` after its next write one count off, in range.
    pub fn corrupt_readback(&self, core: CoreId) {
        self.shared.borrow_mut().faults.corrupt_armed.insert(core);
    }

    /// Fails the next `starts` trial starts.
    pub fn fail_setup(&self, starts: u32) {
        self.shared.borrow_mut().faults.fail_setup = starts;
    }

    /// Makes every start of a trial that runs `backend` fail its setup, an inconclusive trial.
    pub fn fail_backend_setup(&self, backend: Backend) {
        self.shared
            .borrow_mut()
            .faults
            .failing_backends
            .insert(backend);
    }

    /// Makes every start of a trial of `backend` fail with a missing backend.
    pub fn miss_backend(&self, backend: Backend) {
        self.shared
            .borrow_mut()
            .faults
            .missing_backends
            .insert(backend);
    }

    /// Makes the next trial that starts report one worker escaped its CPUs.
    pub fn escape(&self) {
        self.shared.borrow_mut().faults.escape = true;
    }

    /// Fails the preflight check `name` with `detail`; an empty detail clears the failure.
    pub fn fail_check(&self, name: &str, detail: &str) {
        let checks = &mut self.shared.borrow_mut().faults.failed_checks;
        if detail.is_empty() {
            checks.remove(name);
        } else {
            checks.insert(name.to_owned(), detail.to_owned());
        }
    }

    pub fn set_bios_context(&self, context: BiosContext) {
        self.shared.borrow_mut().bios = context;
    }

    /// Pins the outcome of trial `trial`, replacing any script it had.
    pub fn script_trial(&self, trial: &str, script: Script) -> Result<(), ScriptError> {
        let mut runtime = self.shared.borrow_mut();
        let cores = runtime.model.cores();
        if let Some(signal) = script.signal
            && failure_signal(Some(signal)).is_err()
        {
            return Err(ScriptError::Signal {
                trial: trial.to_owned(),
                signal,
            });
        }
        if script.core.index() >= cores {
            return Err(ScriptError::Core {
                trial: trial.to_owned(),
                core: script.core,
                cores,
            });
        }
        if !(script.at_s.is_finite() && script.at_s >= 0.0) {
            return Err(ScriptError::At {
                trial: trial.to_owned(),
                at_s: script.at_s,
            });
        }
        runtime.scripts.insert(trial.to_owned(), script);
        Ok(())
    }

    /// Calls `hook` before and after every actual SMU access. See [`SmuHook`].
    pub fn interrupt_smu(&self, hook: impl SmuHook + 'static) {
        self.shared.borrow_mut().faults.hook = Some(Box::new(hook));
    }

    /// Writes each trial's samples under `dir/<trial id>/samples.jsonl`, compressed by
    /// [`Trials::ended`](togi_machine::Trials::ended), instead of keeping the last trial's in memory. Setting the
    /// directory it already has keeps the last trial's samples.
    pub fn set_samples_dir(&self, dir: Option<PathBuf>) {
        let mut runtime = self.shared.borrow_mut();
        if runtime.samples_dir != dir || runtime.sample_storage.is_some() {
            runtime.samples_dir = dir;
            runtime.sample_storage = None;
            runtime.samples = crate::samples::LastTrial::default();
        }
    }

    /// Attaches an injectable sample store, clearing samples retained by the previous store.
    pub fn set_sample_storage(&self, storage: std::rc::Rc<dyn crate::SampleStorage>) {
        let mut runtime = self.shared.borrow_mut();
        runtime.samples_dir = None;
        runtime.samples = crate::samples::LastTrial::default();
        runtime.sample_storage = Some(storage);
    }

    /// Forgets the last trial's samples, so a trial of a new session that reuses its id reads none.
    pub fn forget_last_trial(&self) {
        self.shared.borrow_mut().samples = crate::samples::LastTrial::default();
    }

    /// The state a later process continues this machine from with [`restore`](Self::restore).
    pub fn persisted(&self) -> Persisted {
        self.shared.borrow().persisted()
    }

    /// The machine `persisted` describes, running `model` and answering from `replay`.
    ///
    /// # Errors
    ///
    /// A persisted state for another number of cores, one without a boot, or an unrepresentable wall clock.
    pub fn restore(
        model: Model,
        replay: Option<Replay>,
        persisted: Persisted,
    ) -> Result<Self, RestoreError> {
        Ok(Self {
            shared: Rc::new(RefCell::new(Runtime::restore(model, replay, persisted)?)),
        })
    }
}
