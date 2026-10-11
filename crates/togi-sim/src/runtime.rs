//! The mutable state of a simulated machine: virtual clocks, boots, registers, kernel logs and armed faults.
//! Seam adapters and the [`Simulator`](crate::Simulator) share it as `Rc<RefCell<Runtime>>`; no borrow is
//! ever held across a reporter or hook call.

use jiff::{SignedDuration, Timestamp};
use rand::RngExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::Duration;
use togi_machine::{
    BankType, BiosContext, BootId, CoreId, CpuId, Mce, ResetKind, ResetReason, TargetOffset,
};

use crate::faults::Faults;
use crate::model::{Model, Script};
use crate::random::{Domain, stream};
use crate::replay::Replay;
use crate::samples::LastTrial;

pub(crate) type Shared = Rc<RefCell<Runtime>>;

/// How long a reboot takes outside any trial's exposure.
pub const REBOOT_TIME: Duration = Duration::from_secs(90);

/// 2026-01-01T00:00:00Z, the start of a machine with no history.
const EPOCH: Timestamp = Timestamp::constant(1_767_225_600, 0);

const BANKS: [BankType; 9] = [
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
const CORE_LOCAL_BANKS: usize = 6;

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

/// A machine check waiting for the next boot to log it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingMce {
    pub core: CoreId,
    pub bank: u16,
    pub bank_type: BankType,
    pub corrected: bool,
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
            model,
        };
        runtime.start_boot();
        runtime
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
        let cpu = CpuId(u16::from(pending.core.get()));
        let line = format!(
            "[Hardware Error]: {kind} error on CPU {} bank {} ({}) at {time} (simulated)",
            cpu.0, pending.bank, pending.bank_type,
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
                BANKS.len() - CORE_LOCAL_BANKS,
                (x - local) / (1.0 - local),
            )
        };
        let bank = first + bank_offset(x, count);
        PendingMce {
            core,
            bank: u16::try_from(bank).unwrap_or(0),
            bank_type: BANKS[bank],
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
fn bank_offset(x: f64, count: usize) -> usize {
    let count = u8::try_from(count).unwrap_or(u8::MAX);
    ((x * f64::from(count)) as usize).min(usize::from(count) - 1)
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
