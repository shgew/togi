mod cancellation;
mod go_json;
mod per_core;
mod samples;
mod scalars;
mod seams;
mod topology;
mod vocab;
mod workloads;

pub use cancellation::CancellationToken;
pub use go_json::{ExceededMaxDepth, json_text};
pub use per_core::PerCore;
pub use samples::{PmTable, PmTableReader, ReadSamples, TrialConditions, read_samples};
pub use scalars::{
    CcdId, CoreId, CpuId, InvalidCore, InvalidTarget, MAX_OFFSET, MIN_OFFSET, NanoDuration,
    ObservedOffset, PER_CORE_MAX, TargetOffset,
};
pub use seams::{
    Check, Clock, CoreRank, Error, ErrorKind, Host, Instance, Kernel, KernelRead, Machine, Mce,
    PstoreRecord, Reporter, ResetReason, Running, Sample, SeamOutcome, SeamResult, Smu, Started,
    TrialResult, TrialSpec, Trials,
};
pub use topology::{BiosContext, CoreInfo, HostText, compare_context, order};
pub use vocab::{
    Backend, BankType, Condition, REGIMES, Regime, ResetKind, Signal, UnknownBackend,
    UnknownBankType, UnknownCondition, UnknownRegime, UnknownResetKind, UnknownSignal,
};
pub use workloads::{MPRIME_AVX512_ID, Workload, pick_workload, workload_by_id, workloads};

#[cfg(test)]
mod tests;
