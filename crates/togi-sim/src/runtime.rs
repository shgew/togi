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
    pub(crate) fn new(model: Model, settings: RunSettings) -> Self {
        let RunSettings {
            seed,
            history,
            replay,
        } = settings;
        let epoch = history
            .last_time
            .and_then(|last| last.checked_add(REBOOT_TIME).ok())
            .unwrap_or(EPOCH);
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
        runtime.start_boot();
        runtime
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
        Ok(Self {
            seed: persisted.seed,
            replay,
            epoch: persisted.epoch,
            clock: persisted.clock,
            boot_at: persisted.boot_at,
            wall_offset: persisted.wall_offset,
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
        })
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

    fn start_boot(&mut self) {
        self.boot_number += 1;
        self.boot_at = self.clock;
        let id = boot_id(self.seed, self.boot_number);
        self.boots.push(id.clone());
        self.regs = self.model.bios().to_vec();
        self.crashed = false;
        self.faults.start_boot();
        for pending in std::mem::take(&mut self.queued) {
            self.log_mce(&id, pending, self.clock);
        }
        let kind = self.next_reset.take();
        self.reasons
            .insert(id, reset_reason(kind, !self.model.old_kernel()));
    }

    pub(crate) fn reboot(&mut self) {
        self.clock += REBOOT_TIME;
        self.start_boot();
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

    pub(crate) fn advance(&mut self, by: Duration) {
        self.clock += by;
    }

    pub(crate) fn set_clock(&mut self, to: Duration) {
        self.clock = self.clock.max(to);
    }

    pub(crate) fn monotonic(&self) -> Duration {
        self.clock.saturating_sub(self.boot_at)
    }

    pub(crate) fn jump_wall(&mut self, by: SignedDuration) {
        self.wall_offset = self.wall_offset.saturating_add(by);
    }

    fn wall_at(&self, clock: Duration) -> Timestamp {
        self.epoch
            .checked_add(clock)
            .and_then(|time| time.checked_add(self.wall_offset))
            .unwrap_or(self.epoch)
    }

    pub(crate) fn wall(&self) -> Timestamp {
        self.wall_at(self.clock)
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
    pub(crate) fn log_mce(&mut self, boot: &BootId, pending: PendingMce, at: Duration) {
        let time = self.wall_at(at);
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
    use super::bank_offset;
    use test_case::test_case;

    #[test_case(0.0, 3, 0; "first shared bank")]
    #[test_case(1.0, 3, 2; "last shared bank")]
    #[test_case(0.0, 6, 0; "first core-local bank")]
    #[test_case(1.0, 6, 5; "last core-local bank")]
    fn bank_draws_stay_inside_the_catalogue(draw: f64, count: u8, expected: u8) {
        assert_eq!(bank_offset(draw, count), expected);
    }
}
