//! The mutable state of a simulated machine: virtual clocks, boots, registers, kernel logs and armed faults.
//! Seam adapters and the [`Simulator`](crate::Simulator) share it as `Rc<RefCell<Runtime>>`; no borrow is
//! ever held across a reporter or hook call.

use jiff::{SignedDuration, Timestamp};
use rand::RngExt;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::Duration;
use togi_machine::{
    BankType, BiosContext, BootId, CoreId, CpuId, Mce, ResetKind, ResetReason, TargetOffset,
    object_only,
};

use crate::faults::Faults;
use crate::model::{Model, Script};
use crate::persisted::{Persisted, PersistedBoot, PersistedMce, RestoreError};
use crate::random::{Domain, stream};
use crate::replay::Replay;
use crate::samples::LastTrial;

pub(crate) type Shared = Rc<RefCell<Runtime>>;

/// How long a reboot takes outside any trial's exposure.
pub const REBOOT_TIME: Duration = Duration::from_secs(90);

/// 2026-01-01T00:00:00Z, the start of a machine with no history.
const EPOCH: Timestamp = Timestamp::constant(1_767_225_600, 0);

const BANK_COUNT: u8 = 9;
const BANKS: [BankType; BANK_COUNT as usize] = [
    BankType::LoadStore,
    BankType::InstructionFetch,
    BankType::L2Cache,
    BankType::DecodeUnit,
    BankType::ExecutionUnit,
    BankType::FloatingPoint,
    BankType::L3Cache,
    BankType::MemoryController,
    BankType::DataFabric,
];
const CORE_LOCAL_BANKS: u8 = 6;

/// What earlier invocations of a simulation left behind, so a resumed machine continues after them.
#[derive(Clone, Debug, Default)]
pub struct History {
    /// Boots before the first of this machine; boot numbering and boot IDs continue from it.
    pub boots: u32,
    /// The wall-clock time of the last recorded event. The machine starts one reboot after it.
    pub last_time: Option<Timestamp>,
    /// The BIOS context of the last recorded session, used when the machine file declares none.
    pub bios_context: Option<BiosContext>,
}

/// Arithmetic that cannot be represented by a simulated machine.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ArithmeticError {
    #[error("add reboot time to the last recorded timestamp")]
    ResumeTime(#[source] jiff::Error),
    #[error("allocate the next simulated boot number")]
    BootNumber,
    #[error("advance the simulated clock")]
    ClockAdvance,
    #[error("add elapsed time to the simulated epoch")]
    WallElapsed(#[source] jiff::Error),
    #[error("apply the simulated wall-clock offset")]
    WallOffset(#[source] jiff::Error),
    #[error("add the simulated wall-clock offset")]
    OffsetAdvance,
}

impl From<ArithmeticError> for togi_machine::Error {
    fn from(error: ArithmeticError) -> Self {
        Self::Other(Box::new(error))
    }
}

/// What differs from one run of a machine to the next, as opposed to the machine itself.
#[derive(Debug, Default)]
pub struct RunSettings {
    /// Seeds every draw of the run: failure times, signals, boot IDs.
    pub seed: u64,
    pub history: History,
    pub replay: Option<Replay>,
}

object_only! {
    /// A machine check waiting for the next boot to log it.
    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct PendingMce {
        pub core: CoreId,
        pub bank: u16,
        pub bank_type: BankType,
        pub corrected: bool,
    }
}

pub(crate) struct Runtime {
    pub model: Model,
    pub seed: u64,
    pub replay: Option<Replay>,
    epoch: Timestamp,
    /// Virtual time since the simulation began; only moves forward.
    clock: Duration,
    boot_at: Duration,
    wall_offset: SignedDuration,
    /// Validated whenever a clock changes, since the machine's `Clock::now` is infallible.
    wall: Timestamp,
    pub boot_number: u32,
    boots: Vec<BootId>,
    pub regs: Vec<TargetOffset>,
    pub crashed: bool,
    logs: HashMap<BootId, Vec<Mce>>,
    queued: Vec<PendingMce>,
    reasons: HashMap<BootId, ResetReason>,
    next_reset: Option<ResetKind>,
    pub bios: BiosContext,
    pub scripts: BTreeMap<String, Script>,
    pub violations: Vec<String>,
    pub faults: Faults,
    pub samples: LastTrial,
    pub samples_dir: Option<std::path::PathBuf>,
    pub sample_storage: Option<std::rc::Rc<dyn crate::SampleStorage>>,
}

impl Runtime {
    pub(crate) fn new(model: Model, settings: RunSettings) -> Result<Self, ArithmeticError> {
        let RunSettings {
            seed,
            history,
            replay,
        } = settings;
        let epoch = match history.last_time {
            Some(last) => last
                .checked_add(REBOOT_TIME)
                .map_err(ArithmeticError::ResumeTime)?,
            None => EPOCH,
        };
        let bios = match history.bios_context {
            Some(recorded) if !model.bios_context_declared() => recorded,
            _ => model.bios_context().clone(),
        };
        let mut runtime = Self {
            seed,
            replay,
            epoch,
            clock: Duration::ZERO,
            boot_at: Duration::ZERO,
            wall_offset: SignedDuration::ZERO,
            wall: epoch,
            boot_number: history.boots,
            boots: Vec::new(),
            regs: Vec::new(),
            crashed: false,
            logs: HashMap::new(),
            queued: Vec::new(),
            reasons: HashMap::new(),
            next_reset: None,
            bios,
            scripts: model.scripts().clone(),
            violations: Vec::new(),
            faults: Faults::default(),
            samples: LastTrial::default(),
            samples_dir: None,
            sample_storage: None,
            model,
        };
        runtime.start_boot(Duration::ZERO)?;
        Ok(runtime)
    }

    pub(crate) fn restore(
        model: Model,
        replay: Option<Replay>,
        persisted: Persisted,
    ) -> Result<Self, RestoreError> {
        if persisted.regs.len() != model.bios().len() {
            return Err(RestoreError::Cores {
                persisted: persisted.regs.len(),
                model: model.bios().len(),
            });
        }
        if persisted.boots.is_empty() {
            return Err(RestoreError::NoBoot);
        }
        let mut boots = Vec::with_capacity(persisted.boots.len());
        let mut logs = HashMap::new();
        let mut reasons = HashMap::new();
        for boot in persisted.boots {
            boots.push(boot.id.clone());
            reasons.insert(boot.id.clone(), boot.reason.into());
            if !boot.mces.is_empty() {
                logs.insert(boot.id, boot.mces.into_iter().map(Mce::from).collect());
            }
        }
        let mut runtime = Self {
            seed: persisted.seed,
            replay,
            epoch: persisted.epoch,
            clock: persisted.clock,
            boot_at: persisted.boot_at,
            wall_offset: persisted.wall_offset,
            wall: persisted.epoch,
            boot_number: persisted.boot_number,
            boots,
            regs: persisted.regs,
            crashed: persisted.crashed,
            logs,
            queued: persisted.queued,
            reasons,
            next_reset: persisted.next_reset,
            bios: persisted.bios,
            scripts: model.scripts().clone(),
            violations: Vec::new(),
            faults: Faults::default(),
            samples: persisted.samples,
            samples_dir: None,
            sample_storage: None,
            model,
        };
        runtime.wall = runtime.wall_at(runtime.clock)?;
        Ok(runtime)
    }

    pub(crate) fn persisted(&self) -> Persisted {
        Persisted {
            seed: self.seed,
            epoch: self.epoch,
            clock: self.clock,
            boot_at: self.boot_at,
            wall_offset: self.wall_offset,
            boot_number: self.boot_number,
            boots: self
                .boots
                .iter()
                .filter_map(|id| {
                    Some(PersistedBoot {
                        id: id.clone(),
                        reason: self.reasons.get(id)?.into(),
                        mces: self.mces(id).iter().map(PersistedMce::from).collect(),
                    })
                })
                .collect(),
            regs: self.regs.clone(),
            crashed: self.crashed,
            queued: self.queued.clone(),
            next_reset: self.next_reset,
            bios: self.bios.clone(),
            samples: self.samples.clone(),
        }
    }

    fn start_boot(&mut self, clock: Duration) -> Result<(), ArithmeticError> {
        let number = self
            .boot_number
            .checked_add(1)
            .ok_or(ArithmeticError::BootNumber)?;
        let wall = self.wall_at(clock)?;
        self.clock = clock;
        self.wall = wall;
        self.boot_number = number;
        self.boot_at = clock;
        let id = boot_id(self.seed, number);
        self.boots.push(id.clone());
        self.regs = self.model.bios().to_vec();
        self.crashed = false;
        self.faults.start_boot();
        for pending in std::mem::take(&mut self.queued) {
            self.log_mce(&id, pending, clock)?;
        }
        let kind = self.next_reset.take();
        self.reasons
            .insert(id, reset_reason(kind, !self.model.old_kernel()));
        Ok(())
    }

    pub(crate) fn reboot(&mut self) -> Result<(), ArithmeticError> {
        let clock = self
            .clock
            .checked_add(REBOOT_TIME)
            .ok_or(ArithmeticError::ClockAdvance)?;
        self.start_boot(clock)
    }

    pub(crate) fn boot_id(&self) -> &BootId {
        &self.boots[self.boots.len() - 1]
    }

    pub(crate) fn has_boot(&self, boot: &BootId) -> bool {
        self.reasons.contains_key(boot)
    }

    pub(crate) fn boot_after(&self, boot: &BootId) -> Option<&BootId> {
        let index = self.boots.iter().position(|known| known == boot)?;
        self.boots.get(index + 1)
    }

    pub(crate) fn reason(&self, boot: &BootId) -> Option<&ResetReason> {
        self.reasons.get(boot)
    }

    pub(crate) fn mces(&self, boot: &BootId) -> &[Mce] {
        self.logs.get(boot).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn clock(&self) -> Duration {
        self.clock
    }

    pub(crate) fn advance(&mut self, by: Duration) -> Result<(), ArithmeticError> {
        let to = self
            .clock
            .checked_add(by)
            .ok_or(ArithmeticError::ClockAdvance)?;
        self.set_clock(to)
    }

    pub(crate) fn set_clock(&mut self, to: Duration) -> Result<(), ArithmeticError> {
        let to = self.clock.max(to);
        let wall = self.wall_at(to)?;
        self.clock = to;
        self.wall = wall;
        Ok(())
    }

    pub(crate) fn monotonic(&self) -> Duration {
        self.clock.saturating_sub(self.boot_at)
    }

    pub(crate) fn jump_wall(&mut self, by: SignedDuration) -> Result<(), ArithmeticError> {
        let offset = checked_wall_offset(self.wall_offset, by)?;
        let wall = self
            .epoch
            .checked_add(self.clock)
            .map_err(ArithmeticError::WallElapsed)?
            .checked_add(offset)
            .map_err(ArithmeticError::WallOffset)?;
        self.wall_offset = offset;
        self.wall = wall;
        Ok(())
    }

    fn wall_at(&self, clock: Duration) -> Result<Timestamp, ArithmeticError> {
        self.epoch
            .checked_add(clock)
            .map_err(ArithmeticError::WallElapsed)?
            .checked_add(self.wall_offset)
            .map_err(ArithmeticError::WallOffset)
    }

    pub(crate) fn wall(&self) -> Timestamp {
        self.wall
    }

    pub(crate) fn crash(&mut self) {
        self.next_reset.get_or_insert(ResetKind::Watchdog);
        self.crashed = true;
    }

    pub(crate) fn set_next_reset(&mut self, kind: ResetKind) {
        self.next_reset = Some(kind);
    }

    pub(crate) const fn next_reset_is_set(&self) -> bool {
        self.next_reset.is_some()
    }

    pub(crate) fn queue_mce(&mut self, pending: PendingMce) {
        self.queued.push(pending);
    }

    /// Logs `pending` in `boot` as of virtual time `at`.
    pub(crate) fn log_mce(
        &mut self,
        boot: &BootId,
        pending: PendingMce,
        at: Duration,
    ) -> Result<(), ArithmeticError> {
        let time = self.wall_at(at)?;
        let kind = if pending.corrected {
            "Corrected"
        } else {
            "Uncorrected"
        };
        let cpu = CpuId::from(u16::from(pending.core.get()));
        let line = format!(
            "[Hardware Error]: {kind} error on CPU {} bank {} ({}) at {time} (simulated)",
            cpu.get(),
            pending.bank,
            pending.bank_type,
        );
        self.logs.entry(boot.clone()).or_default().push(Mce {
            cpu,
            core: Some(pending.core),
            bank: pending.bank,
            bank_type: pending.bank_type,
            corrected: pending.corrected,
            time,
            monotonic: at.saturating_sub(self.boot_at),
            lines: vec![line],
        });
        Ok(())
    }

    /// The machine check a draw `x` in `[0, 1)` names on `core`: a core-local bank with the model's
    /// probability, else a shared one.
    pub(crate) fn mce(&self, x: f64, core: CoreId, corrected: bool) -> PendingMce {
        let local = self.model.params().core_local_bank;
        let (first, count, x) = if x < local {
            (0, CORE_LOCAL_BANKS, x / local)
        } else {
            (
                CORE_LOCAL_BANKS,
                BANK_COUNT - CORE_LOCAL_BANKS,
                (x - local) / (1.0 - local),
            )
        };
        let bank = first + bank_offset(x, count);
        PendingMce {
            core,
            bank: u16::from(bank),
            bank_type: BANKS[usize::from(bank)],
            corrected,
        }
    }

    pub(crate) fn draw_reset(&self, x: f64) -> ResetKind {
        self.model.reset_mix().pick(x)
    }
}

fn checked_wall_offset(
    offset: SignedDuration,
    by: SignedDuration,
) -> Result<SignedDuration, ArithmeticError> {
    offset.checked_add(by).ok_or(ArithmeticError::OffsetAdvance)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "x is in [0, 1], so the product is a small nonnegative whole number"
)]
fn bank_offset(x: f64, count: u8) -> u8 {
    ((x * f64::from(count)) as u8).min(count - 1)
}

fn boot_id(seed: u64, number: u32) -> BootId {
    let mut rng = stream(seed, Domain::Boot, &[u64::from(number)]);
    let hex = format!("{:016x}{:016x}", rng.random::<u64>(), rng.random::<u64>());
    BootId::from(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// What a kernel of this age logs about the reset that preceded a boot.
fn reset_reason(kind: Option<ResetKind>, supported: bool) -> ResetReason {
    if !supported {
        return ResetReason {
            kind: None,
            raw: Vec::new(),
            supported: false,
        };
    }
    let described = match kind {
        Some(ResetKind::Watchdog) => Some((1, "hardware watchdog timer expired")),
        Some(ResetKind::SyncFlood) => Some((
            2,
            "an uncorrected error caused a data fabric sync flood event",
        )),
        Some(ResetKind::CpuShutdown) => Some((3, "internal CPU shutdown event occurred")),
        Some(ResetKind::PowerButton) => Some((4, "power button was pressed for 4 seconds")),
        Some(ResetKind::ThermalTrip) => Some((5, "thermal pin BP_THERMTRIP_L was tripped")),
        Some(ResetKind::Unknown) => Some((6, "unrecognized reset reason")),
        Some(ResetKind::PowerLoss) | None => None,
    };
    ResetReason {
        kind: kind.filter(|&kind| kind != ResetKind::PowerLoss),
        raw: described
            .map(|(code, text)| {
                format!("x86/amd: Previous system reset reason [0x{code:08x}]: {text}")
            })
            .into_iter()
            .collect(),
        supported: true,
    }
}

#[cfg(test)]
mod numeric_tests {
    use super::{ArithmeticError, bank_offset, checked_wall_offset};
    use jiff::SignedDuration;
    use test_case::test_case;

    #[test_case(0.0, 3, 0; "first shared bank")]
    #[test_case(1.0, 3, 2; "last shared bank")]
    #[test_case(0.0, 6, 0; "first core-local bank")]
    #[test_case(1.0, 6, 5; "last core-local bank")]
    fn bank_draws_stay_inside_the_catalogue(draw: f64, count: u8, expected: u8) {
        assert_eq!(bank_offset(draw, count), expected);
    }

    #[test_case(SignedDuration::ZERO, SignedDuration::ZERO, Some(SignedDuration::ZERO); "zero")]
    #[test_case(SignedDuration::MAX, SignedDuration::from_nanos(1), None; "positive overflow")]
    #[test_case(SignedDuration::MIN, SignedDuration::from_nanos(-1), None; "negative overflow")]
    fn wall_corrections_refuse_both_signed_limits(
        offset: SignedDuration,
        by: SignedDuration,
        expected: Option<SignedDuration>,
    ) {
        let result = checked_wall_offset(offset, by);
        match expected {
            Some(expected) => assert_eq!(result.unwrap(), expected),
            None => assert!(matches!(result, Err(ArithmeticError::OffsetAdvance))),
        }
    }
}
