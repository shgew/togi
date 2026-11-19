//! Inputs that vary between the fitted research model and a deterministic mechanics script.

use std::rc::Rc;
use std::time::Duration;

use togi_machine::{BiosContext, CoreId, ResetKind, TargetOffset, TrialSpec};

use crate::Model;
use crate::definition::{
    DEFAULT_BIOS_VERSION, DEFAULT_BOARD, DEFAULT_BOOST_LIMIT_MHZ, DEFAULT_MICROCODE,
};
use crate::replay::Replay;

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
    SixteenCores,
}

impl ScriptedTopology {
    const fn cores(self) -> usize {
        match self {
            Self::TwoCores => 2,
            Self::FourCores => 4,
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
    stock: TargetOffset,
    pub(crate) outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
}

impl ScriptedMachine {
    pub const fn new(
        topology: ScriptedTopology,
        stock: TargetOffset,
        outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
    ) -> Self {
        Self {
            topology,
            stock,
            outcome,
        }
    }
}

/// What the script makes one simulated trial do. The times are relative to its start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrialOutcome {
    Pass,
    ComputationError {
        core: CoreId,
        at: Duration,
    },
    Reset {
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

impl MachineSource {
    pub fn fitted(model: Model, replay: Option<Replay>) -> Self {
        Self::Fitted {
            model,
            replay: replay.map(Rc::new),
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
            Self::Scripted(script) => registers.resize(script.topology.cores(), script.stock),
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

    pub(crate) fn has_voltage_lanes(&self) -> bool {
        match self {
            Self::Fitted { model, .. } => model.voltage().is_some(),
            Self::Scripted(_) => false,
        }
    }

    pub(crate) fn supports_reset_reasons(&self) -> bool {
        match self {
            Self::Fitted { model, .. } => !model.old_kernel(),
            Self::Scripted(_) => true,
        }
    }
}
