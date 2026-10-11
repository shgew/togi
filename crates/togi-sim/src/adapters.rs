//! The clock, SMU, host and kernel seams of a simulated machine.

use jiff::Timestamp;
use std::time::Duration;
use togi_machine::{
    BiosContext, BootId, CancellationToken, Check, Clock, CoreId, CoreInfo, CoreRank, CpuId, Error,
    Host, Kernel, KernelCursor, KernelRead, MIN_OFFSET, Mce, ObservedOffset, Partial, PstoreRecord,
    ResetReason, Smu, SmuValidated, Stopped, TargetOffset,
};

use crate::cores::ids;
use crate::faults::{HookPhase, SmuOp, SmuOperation};
use crate::runtime::{Runtime, Shared};

fn other(message: impl Into<String>) -> Error {
    Error::Other(message.into().into())
}

impl Runtime {
    pub(crate) fn alive(&self) -> Result<(), Error> {
        if self.crashed {
            Err(Error::Crashed)
        } else {
            Ok(())
        }
    }

    pub(crate) fn existing_core(&self, core: CoreId) -> Result<(), Error> {
        if core.index() < self.regs.len() {
            Ok(())
        } else {
            Err(other(format!("simulated SMU: no core {core}")))
        }
    }

    /// Whether the machine is still alive in boot `boot`: a reentrant hook may have crashed or rebooted it.
    fn still(&self, boot: u32) -> Result<(), Error> {
        self.alive()?;
        if self.boot_number == boot {
            Ok(())
        } else {
            Err(Error::Crashed)
        }
    }

    /// Everything that can stop a write before it reaches a register: a crash, and the armed write faults.
    fn gate_write(&mut self) -> Result<(), Error> {
        self.alive()?;
        let first = !self.faults.wrote_this_boot;
        if self.faults.crash_write_in > 0 {
            self.faults.crash_write_in -= 1;
            if self.faults.crash_write_in == 0 {
                self.crash();
                return Err(Error::Crashed);
            }
        }
        if self.faults.fail_write_in > 0 {
            self.faults.fail_write_in -= 1;
            if self.faults.fail_write_in == 0 {
                return Err(other("simulated SMU command failure"));
            }
        }
        self.faults.wrote_this_boot = true;
        if first && self.faults.crash_before_apply > 0 {
            self.faults.crash_before_apply -= 1;
            self.crash();
            return Err(Error::Crashed);
        }
        Ok(())
    }

    fn written(&mut self, core: CoreId, offset: TargetOffset) {
        self.regs[core.index()] = offset;
        if self.faults.corrupt_armed.remove(core) {
            self.faults.corrupt_pending.insert(core);
        }
    }
}

/// Calls the hook, if any, with no borrow of the runtime held: the hook may use the simulator.
fn intercept(
    shared: &Shared,
    op: SmuOp,
    core: Option<CoreId>,
    offset: i64,
    phase: HookPhase,
) -> Result<(), Error> {
    let Some(mut hook) = shared.borrow_mut().faults.hook.take() else {
        return Ok(());
    };
    let result = hook.intercept(&SmuOperation {
        op,
        core,
        offset,
        phase,
    });
    let mut runtime = shared.borrow_mut();
    if runtime.faults.hook.is_none() {
        runtime.faults.hook = Some(hook);
    }
    result
}

pub(crate) struct SimClock(pub(crate) Shared);

impl Clock for SimClock {
    fn now(&self) -> Timestamp {
        self.0.borrow().wall()
    }

    fn monotonic(&self) -> Duration {
        self.0.borrow().monotonic()
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut runtime = self.0.borrow_mut();
        runtime.alive()?;
        runtime.advance(duration);
        Ok(())
    }
}

pub(crate) struct SimSmu(pub(crate) Shared);

impl Smu for SimSmu {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error> {
        let (current, boot) = {
            let runtime = self.0.borrow();
            runtime.alive()?;
            runtime.existing_core(core)?;
            (runtime.regs[core.index()].get(), runtime.boot_number)
        };
        intercept(&self.0, SmuOp::Read, Some(core), current, HookPhase::Before)?;
        let observed = {
            let mut runtime = self.0.borrow_mut();
            runtime.still(boot)?;
            let register = runtime.regs[core.index()].get();
            if runtime.faults.corrupt_pending.remove(core) {
                if register == MIN_OFFSET {
                    register + 1
                } else {
                    register - 1
                }
            } else {
                register
            }
        };
        intercept(&self.0, SmuOp::Read, Some(core), observed, HookPhase::After)?;
        Ok(ObservedOffset(observed))
    }

    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error> {
        let boot = {
            let mut runtime = self.0.borrow_mut();
            runtime.alive()?;
            runtime.existing_core(core)?;
            runtime.gate_write()?;
            runtime.boot_number
        };
        let value = offset.get();
        intercept(&self.0, SmuOp::Set, Some(core), value, HookPhase::Before)?;
        {
            let mut runtime = self.0.borrow_mut();
            runtime.still(boot)?;
            runtime.written(core, offset);
        }
        intercept(&self.0, SmuOp::Set, Some(core), value, HookPhase::After)
    }

    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
        write_all(&self.0, offset)
    }

    fn emergency_zero(
        &mut self,
        _validated: SmuValidated,
        _stopped: &Stopped,
    ) -> Result<(), Error> {
        write_all(&self.0, TargetOffset::ZERO)
    }
}

fn write_all(shared: &Shared, offset: TargetOffset) -> Result<(), Error> {
    let boot = {
        let mut runtime = shared.borrow_mut();
        runtime.gate_write()?;
        runtime.boot_number
    };
    let value = offset.get();
    intercept(shared, SmuOp::SetAll, None, value, HookPhase::Before)?;
    {
        let mut runtime = shared.borrow_mut();
        runtime.still(boot)?;
        let cores = runtime.regs.len();
        for core in ids(cores) {
            runtime.written(core, offset);
        }
    }
    intercept(shared, SmuOp::SetAll, None, value, HookPhase::After)
}

#[expect(
    clippy::disallowed_methods,
    reason = "the simulator is the host that validates its own SMU interface"
)]
fn validated() -> SmuValidated {
    SmuValidated::checked()
}

const CHECKS: [&str; 7] = [
    "root",
    "cpu",
    "ryzen_smu",
    "readback",
    "slot_mapping",
    "backends",
    "systemd_run",
];

pub(crate) struct SimHost(pub(crate) Shared);

impl Host for SimHost {
    fn boot_id(&mut self) -> Result<BootId, Error> {
        let runtime = self.0.borrow();
        runtime.alive()?;
        Ok(runtime.boot_id().clone())
    }

    fn topology(&mut self) -> Result<Vec<CoreInfo>, Error> {
        let runtime = self.0.borrow();
        runtime.alive()?;
        let cores = runtime.model.cores();
        Ok(ids(cores)
            .map(|core| {
                let index = core.index();
                let cpu = |index: usize| CpuId(u16::try_from(index).unwrap_or(u16::MAX));
                CoreInfo {
                    core,
                    ccd: togi_machine::CcdId(
                        u8::try_from(runtime.model.ccd_of(core)).unwrap_or(u8::MAX),
                    ),
                    cpus: vec![cpu(index), cpu(index + cores)],
                }
            })
            .collect())
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, Error> {
        let runtime = self.0.borrow();
        for name in ["cpu", "ryzen_smu"] {
            if let Some(detail) = runtime.faults.failed_checks.get(name) {
                return Err(other(format!("validate {name}: {detail}")));
            }
        }
        Ok(validated())
    }

    fn bios_context(&mut self) -> Result<BiosContext, Error> {
        let runtime = self.0.borrow();
        runtime.alive()?;
        Ok(runtime.bios.clone())
    }

    fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        let runtime = self.0.borrow();
        let fail = |error| Partial {
            value: Vec::new(),
            error,
        };
        runtime.alive().map_err(fail)?;
        let ranking = runtime
            .model
            .ranking()
            .ok_or_else(|| fail(other("simulated preferred-core ranking unavailable")))?;
        Ok(ids(ranking.len())
            .zip(ranking)
            .map(|(core, &value)| CoreRank { core, value })
            .collect())
    }

    fn preflight(&mut self) -> Vec<Check> {
        let runtime = self.0.borrow();
        let mut checks: Vec<Check> = CHECKS
            .iter()
            .map(|&name| failed_or(&runtime, name, "simulated"))
            .collect();
        checks.push(Check {
            name: "pm_table".to_owned(),
            detail: if runtime.model.voltage().is_some() {
                "simulated shared-voltage model supplies per-core voltage requests and loaded-core clocks"
            } else {
                "pm_table version unavailable: simulator reports no per-core lanes"
            }
            .to_owned(),
            ok: true,
        });
        checks
    }

    fn watchdog(&mut self) -> Check {
        failed_or(
            &self.0.borrow(),
            "watchdog",
            "simulated hardware watchdog active",
        )
    }
}

fn failed_or(runtime: &Runtime, name: &str, detail: &str) -> Check {
    match runtime.faults.failed_checks.get(name) {
        Some(failure) => Check {
            name: name.to_owned(),
            detail: failure.clone(),
            ok: false,
        },
        None => Check {
            name: name.to_owned(),
            detail: detail.to_owned(),
            ok: true,
        },
    }
}

pub(crate) struct SimKernel(pub(crate) Shared);

impl SimKernel {
    fn known(&self, boot: &BootId) -> Result<(), Error> {
        let runtime = self.0.borrow();
        runtime.alive()?;
        if runtime.has_boot(boot) {
            Ok(())
        } else {
            Err(Error::BootMissing)
        }
    }
}

impl Kernel for SimKernel {
    fn mces(&mut self, boot: &BootId, since: Duration) -> Result<Vec<Mce>, Error> {
        self.known(boot)?;
        let runtime = self.0.borrow();
        Ok(runtime
            .mces(boot)
            .iter()
            .filter(|mce| mce.monotonic >= since)
            .cloned()
            .collect())
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<Mce>>> {
        let fail = |error| Partial {
            value: Vec::new(),
            error,
        };
        self.known(boot).map_err(fail)?;
        let runtime = self.0.borrow();
        let log = runtime.mces(boot);
        let start = match after {
            None => 0,
            Some(cursor) => cursor
                .as_str()
                .strip_prefix(boot.as_str())
                .and_then(|rest| rest.strip_prefix(':'))
                .and_then(|index| index.parse::<usize>().ok())
                .filter(|&index| index <= log.len())
                .ok_or_else(|| fail(Error::CursorMissing))?,
        };
        Ok(KernelRead {
            mces: log[start..].to_vec(),
            cursor: KernelCursor::from(format!("{boot}:{}", log.len())),
        })
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        self.known(boot)?;
        let runtime = self.0.borrow();
        runtime.reason(boot).cloned().ok_or(Error::BootMissing)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        self.0.borrow().alive()?;
        let runtime = self.0.borrow();
        let next = runtime.boot_after(boot).ok_or(Error::BootMissing)?;
        runtime.reason(next).cloned().ok_or(Error::BootMissing)
    }

    fn saved_pstore(&mut self, _boot: &BootId) -> Result<Option<PstoreRecord>, Error> {
        self.0.borrow().alive()?;
        Ok(None)
    }
}
