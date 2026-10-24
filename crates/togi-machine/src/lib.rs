//! The machine vocabulary and interfaces: core identities, checked offsets, profiles' per-core
//! storage and workloads, with [`Machine`], [`Smu`], [`Kernel`] and [`Trials`] as execution boundaries.

mod cancellation;
mod controller;
mod core_set;
mod cpufreq;
mod error;
mod evidence;
mod ids;
mod load;
mod per_core;
mod roles;
mod samples;
mod scalars;
mod seams;
pub mod serde_object;
pub mod serde_pairs;
mod topology;
mod vocab;
mod workloads;

pub use cancellation::{CancellationToken, OnCancel};
pub use controller::{ControllerContract, ControllerMode};
pub use core_set::CoreSet;
pub use cpufreq::{CpuFrequency, FrequencyPolicy, FrequencyPolicyId};
pub use error::{Error, Partial, error_chain};
pub use evidence::{
    EndEvidence, GraphicsDriver, KernelFault, KernelTail, MAX_FAULTS, MceCount, SleepMode,
};
pub use ids::{BootId, KernelCursor};
pub use load::{DutyPct, InvalidDutyPct, LoadSchedule, Toggles};
pub use per_core::PerCore;
pub use roles::{InvalidRoles, PhaseSchedule, TrialRole};
pub use samples::{PmTable, PmTableReader, ReadSamples, SampleError, TrialConditions};
pub use scalars::{
    CcdId, CoreId, CpuId, InvalidCcd, InvalidCore, InvalidCpu, InvalidTarget, MAX_OFFSET,
    MIN_OFFSET, ObservedOffset, PER_CORE_MAX, TargetOffset,
};
pub use seams::{
    Check, Clock, CoreRank, CpuTime, Host, Instance, Kernel, KernelError, KernelErrorKind,
    KernelRead, Machine, Mce, ProcessFault, ProcessFaultKind, PstoreRecord, Reporter, ResetReason,
    Running, Sample, Smu, SmuValidated, Started, Stopped, Swept, TrialFailure, TrialResult,
    TrialSpec, Trials, WatchdogState, monotonic_duration,
};
pub use topology::{BiosContext, ContextField, ContextMismatch, CoreInfo, order};
pub use vocab::{
    ALL_REGIMES, Backend, BankType, Condition, R8_DURATION, R9_DURATION, R9_PHASE, R9_PHASE_S,
    REGIMES, ROLE_WARM_UP, Regime, ResetKind, Signal,
};
pub use workloads::{
    MPRIME_AVX512_ID, UnknownWorkload, Workload, WorkloadId, pick_workload, workload_by_id,
    workloads,
};

#[cfg(test)]
mod tests;
