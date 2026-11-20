//! Inputs that vary between the fitted research model and a deterministic mechanics script.

use std::rc::Rc;
use std::time::Duration;

use togi_machine::{BankType, BiosContext, CoreId, CoreSet, ResetKind, Signal, TargetOffset, TrialSpec};

use crate::Model;
use crate::definition::{
    DEFAULT_BIOS_VERSION, DEFAULT_BOARD, DEFAULT_BOOST_LIMIT_MHZ, DEFAULT_MICROCODE,
};
use crate::persisted::PersistedSource;
use crate::replay::Replay;
use crate::voltage::VoltageState;

/// The inputs of a fake machine, supplied again when restoring its mutable state.
#[derive(Clone, Debug)]
pub enum MachineSource {
    Fitted {
        model: Model,
        replay: Option<Rc<Replay>>,
    },
    Scripted(ScriptedMachine),
}

/// The two-CCD topologies used by the scripted fixtures.
#[derive(Clone, Copy, Debug)]
pub enum ScriptedTopology {
    TwoCores,
    FourCores,
    EightCores,
    SixteenCores,
}

impl ScriptedTopology {
    const fn cores(self) -> usize {
        match self {
            Self::TwoCores => 2,
            Self::FourCores => 4,
            Self::EightCores => 8,
            Self::SixteenCores => 16,
        }
    }
}

/// A fake machine that answers trials from code, without a hazard, replay oracle or fitted limits.
/// The script sees the typed trial (including its ordinal and controller placement request) and
/// the actual per-core registers. It must give the same answer after restoring the machine.
#[derive(Clone, Copy, Debug)]
pub struct ScriptedMachine {
    topology: ScriptedTopology,
    stock: BootOffsets,
    voltage: Option<&'static ScriptedVoltageSamples>,
    pub(crate) outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
}

#[derive(Clone, Copy, Debug)]
enum BootOffsets {
    Uniform(TargetOffset),
    PerCore(fn(CoreId) -> TargetOffset),
}

/// Fixed PM-table requests and clocks for a sixteen-core fixture; not used to choose outcomes.
#[derive(Debug)]
pub struct ScriptedVoltageSamples {
    pub requests_v: [f32; 16],
    pub ccd_mhz: [u32; 2],
}

impl ScriptedMachine {
    pub const fn new(
        topology: ScriptedTopology,
        stock: TargetOffset,
        outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
    ) -> Self {
        Self {
            topology,
            stock: BootOffsets::Uniform(stock),
            voltage: None,
            outcome,
        }
    }

    /// Uses independently specified reset registers at construction and at every reboot.
    #[must_use]
    pub const fn with_boot_offsets(mut self, offset: fn(CoreId) -> TargetOffset) -> Self {
        self.stock = BootOffsets::PerCore(offset);
        self
    }

    /// A sixteen-core script that supplies fixed voltage requests and CCD clocks.
    pub const fn sixteen_with_voltage_samples(
        stock: TargetOffset,
        outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
        samples: &'static ScriptedVoltageSamples,
    ) -> Self {
        Self {
            voltage: Some(samples),
            ..Self::new(ScriptedTopology::SixteenCores, stock, outcome)
        }
    }

    pub(crate) fn voltage_state(&self, loaded: CoreSet) -> Option<VoltageState> {
        let samples = self.voltage?;
        Some(VoltageState {
            requests: samples.requests_v,
            clocks: samples.ccd_mhz,
            rail: loaded
                .iter()
                .map(|core| f64::from(samples.requests_v[core.index()]))
                .fold(0.0, f64::max),
            rates: [0.0; 16],
        })
    }
}

/// Where a scripted machine check is reported, independently of the workload's stopped worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScriptedMce {
    pub core: CoreId,
    pub bank: u16,
    pub bank_type: BankType,
}

/// What the script makes one simulated trial do. The times are relative to its start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrialOutcome {
    Pass,
    ComputationError {
        core: CoreId,
        at: Duration,
    },
    UnexpectedExit {
        core: CoreId,
        at: Duration,
    },
    Stall {
        core: CoreId,
        at: Duration,
    },
    /// A durable backend error report before the machine disappears, not a completed trial.
    ReportedFailureThenReset {
        core: CoreId,
        failure: ReportedFailure,
        at: Duration,
    },
    /// The kernel records a corrected check while the workload runs its full duration.
    CorrectedMce {
        mce: ScriptedMce,
        at: Duration,
    },
    Reset {
        stalled: Option<CoreId>,
        kind: ResetKind,
        at: Duration,
    },
    /// Independent reset and successor-boot kernel evidence, without an in-trial MCE report.
    ResetWithMce {
        mce: ScriptedMce,
        stalled: Option<CoreId>,
        kind: ResetKind,
        at: Duration,
    },
    /// A named load/store machine check and immediate sync-flood reset.
    UncorrectedMce {
        core: CoreId,
        at: Duration,
    },
}

/// A backend signal that can be journaled immediately before a reset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportedFailure {
    ComputationError,
    Stall,
}

impl ReportedFailure {
    pub(crate) const fn signal(self) -> Signal {
        match self {
            Self::ComputationError => Signal::ComputationError,
            Self::Stall => Signal::Stall,
        }
    }
}

impl MachineSource {
    pub fn fitted(model: Model, replay: Option<Replay>) -> Self {
        Self::Fitted {
            model,
            replay: replay.map(Rc::new),
        }
    }

    pub(crate) const fn persisted_kind(&self) -> PersistedSource {
        match self {
            Self::Fitted { .. } => PersistedSource::Fitted,
            Self::Scripted(_) => PersistedSource::Scripted,
        }
    }

    pub(crate) fn cores(&self) -> usize {
        match self {
            Self::Fitted { model, .. } => model.cores(),
            Self::Scripted(script) => script.topology.cores(),
        }
    }

    pub(crate) fn ccd_of(&self, core: CoreId) -> usize {
        match self {
            Self::Fitted { model, .. } => model.ccd_of(core),
            Self::Scripted(script) => core.index() / (script.topology.cores() / 2),
        }
    }

    pub(crate) fn reset_registers(&self, registers: &mut Vec<TargetOffset>) {
        registers.clear();
        match self {
            Self::Fitted { model, .. } => registers.extend_from_slice(model.bios()),
            Self::Scripted(script) => match script.stock {
                BootOffsets::Uniform(offset) => registers.resize(script.topology.cores(), offset),
                BootOffsets::PerCore(offset) => {
                    registers.extend(crate::cores::ids(script.topology.cores()).map(offset));
                }
            },
        }
    }

    pub(crate) fn bios_context(&self, recorded: Option<BiosContext>) -> BiosContext {
        match self {
            Self::Fitted { model, .. } => match recorded {
                Some(recorded) if !model.bios_context_declared() => recorded,
                _ => model.bios_context().clone(),
            },
            Self::Scripted(script) => recorded.unwrap_or_else(|| BiosContext {
                bios_version: DEFAULT_BIOS_VERSION.into(),
                board: DEFAULT_BOARD.into(),
                cpu_model: format!("Simulated Zen 5 {}-Core Processor", script.topology.cores()),
                microcode: DEFAULT_MICROCODE.into(),
                boost_limit_mhz: DEFAULT_BOOST_LIMIT_MHZ,
            }),
        }
    }

    pub(crate) fn ranking(&self) -> Option<&[i64]> {
        match self {
            Self::Fitted { model, .. } => model.ranking(),
            Self::Scripted(_) => None,
        }
    }

    pub(crate) fn voltage_lanes_detail(&self) -> &'static str {
        match self {
            Self::Fitted { model, .. } if model.voltage().is_some() => {
                "simulated shared-voltage model supplies per-core voltage requests and loaded-core clocks"
            }
            Self::Scripted(script) if script.voltage.is_some() => {
                "scripted samples supply per-core voltage requests and loaded-core clocks"
            }
            Self::Fitted { .. } | Self::Scripted(_) => {
                "pm_table version unavailable: simulator reports no per-core lanes"
            }
        }
    }

    pub(crate) fn supports_reset_reasons(&self) -> bool {
        match self {
            Self::Fitted { model, .. } => !model.old_kernel(),
            Self::Scripted(_) => true,
        }
    }
}
