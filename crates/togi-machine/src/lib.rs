//! The machine vocabulary and interfaces: core identities, checked offsets, profiles' per-core
//! storage and workloads, with [`Machine`], [`Smu`], [`Kernel`] and [`Trials`] as execution boundaries.

mod cancellation;
mod core_set;
mod error;
mod ids;
mod load;
mod per_core;
mod samples;
mod scalars;
mod seams;
pub mod serde_object;
pub mod serde_pairs;
mod topology;
mod vocab;
mod workloads;

pub use cancellation::{CancellationToken, OnCancel};
pub use core_set::CoreSet;
pub use error::{Error, Partial, error_chain};
pub use ids::{BootId, KernelCursor};
pub use load::{DutyPct, InvalidDutyPct, LoadSchedule, Toggles};
pub use per_core::PerCore;
pub use samples::{PmTable, PmTableReader, ReadSamples, SampleError, TrialConditions};
pub use scalars::{
    CcdId, CoreId, CpuId, InvalidCcd, InvalidCore, InvalidCpu, InvalidTarget, MAX_OFFSET,
    MIN_OFFSET, ObservedOffset, PER_CORE_MAX, TargetOffset,
};
pub use seams::{
    Check, Clock, CoreRank, Host, Instance, Kernel, KernelRead, Machine, Mce, PstoreRecord,
    Reporter, ResetReason, Running, Sample, Smu, SmuValidated, Started, Stopped, Swept,
    TrialFailure, TrialResult, TrialSpec, Trials,
};
pub use topology::{BiosContext, ContextField, ContextMismatch, CoreInfo, order};
pub use vocab::{Backend, BankType, Condition, REGIMES, Regime, ResetKind, Signal};
pub use workloads::{
    MPRIME_AVX512_ID, UnknownWorkload, Workload, WorkloadId, pick_workload, workload_by_id,
    workloads,
};

#[cfg(test)]
mod tests;
