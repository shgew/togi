use crate::{
    BankType, BiosContext, CancellationToken, Condition, CoreId, CoreInfo, CpuId, ObservedOffset,
    Regime, ResetKind, Signal, TargetOffset, TrialConditions, Workload,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Why a machine seam call failed. The named variants are the conditions callers branch on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the simulated machine crashed")]
    Crashed,
    #[error("backend binary missing")]
    BackendMissing,
    #[error("trial cleanup could not be confirmed")]
    Containment,
    #[error("boot missing from the system journal")]
    BootMissing,
    #[error("kernel log cursor cannot be resumed")]
    CursorMissing,
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub trait Clock {
    fn now(&self) -> Timestamp;
    /// Time since boot.
    fn monotonic(&self) -> Duration;
    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error>;
}

pub trait Smu {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error>;
    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error>;
    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error>;
}

/// Preferred-core values read before `error` ended the read, in topology order.
#[derive(Debug, Default)]
pub struct Ranking {
    pub ranks: Vec<CoreRank>,
    pub error: Option<Error>,
}

pub trait Host {
    fn boot_id(&mut self) -> Result<String, Error>;
    fn topology(&mut self) -> Result<Vec<CoreInfo>, Error>;
    fn validate_smu(&mut self) -> Result<(), Error>;
    fn bios_context(&mut self) -> Result<BiosContext, Error>;
    fn ranking(&mut self) -> Ranking;
    fn preflight(&mut self) -> Vec<Check>;
    fn watchdog(&mut self) -> Check;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreRank {
    pub core: CoreId,
    pub value: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Check {
    pub name: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialSpec {
    pub id: String,
    pub regime: Regime,
    pub workload: Workload,
    pub condition: Condition,
    pub cores: Vec<CoreId>,
    pub cpus: Vec<CpuId>,
    pub duration: Duration,
    pub index: usize,
    pub seed: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Started {
    pub pid: i64,
    pub scope: String,
    pub cpus: Vec<CpuId>,
    pub argv: Vec<String>,
    pub files: Vec<String>,
    pub instances: Vec<Instance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialResult {
    pub ran: Duration,
    pub signal: Option<Signal>,
    pub core: CoreId,
    pub inconclusive: String,
    pub escaped: Vec<CpuId>,
    pub tctl_max_c: Option<i64>,
    pub stops: i64,
    pub conts: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instance {
    pub core: CoreId,
    pub cpus: Vec<CpuId>,
    pub pid: i64,
    pub scope: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Sample {
    pub warning: String,
    pub pid: i64,
    pub tid: i64,
    pub cpu: CpuId,
}

pub trait Reporter {
    fn progress(&mut self, detail: &str);
    fn sample(&mut self, sample: Sample);
    fn signal(&mut self, core: CoreId, signal: Signal, detail: &str);
}

/// What a trial accumulated before `error`, if any, ended the wait.
#[derive(Debug)]
pub struct Waited {
    pub result: TrialResult,
    pub error: Option<Error>,
}

pub trait Running {
    fn started(&self) -> Started;
    /// Calls the borrowed reporter only on this thread, and joins the workload before returning.
    fn wait(&mut self, cancel: &CancellationToken, report: &mut dyn Reporter) -> Waited;
    /// Cancels and joins; repeated calls return the first teardown result.
    fn stop(&mut self) -> Result<(), Error>;
}

pub trait Trials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<String, Error>;
    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
    ) -> Result<Box<dyn Running>, Error>;
    fn ended(&mut self, id: &str) -> Result<(), Error>;
    fn samples<'a>(&'a self, id: &'a str) -> Box<dyn Iterator<Item = TrialConditions> + 'a>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mce {
    pub cpu: CpuId,
    pub core: Option<CoreId>,
    pub bank: i64,
    pub bank_type: BankType,
    pub corrected: bool,
    pub time: Timestamp,
    pub monotonic: Duration,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetReason {
    pub kind: ResetKind,
    pub raw: String,
    pub supported: bool,
}

/// Machine-check events and the last cursor read before `error`, if any, ended the read.
#[derive(Debug, Default)]
pub struct KernelRead {
    pub mces: Vec<Mce>,
    pub cursor: String,
    pub error: Option<Error>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PstoreRecord {
    pub path: String,
    pub lines: Vec<String>,
}

pub trait Kernel {
    /// An earlier boot missing from the journal is `Error::BootMissing`.
    fn mces(&mut self, boot: &str, since: Duration) -> Result<Vec<Mce>, Error>;
    /// An unresumable cursor is `Error::CursorMissing` in `KernelRead::error`; the MCEs and cursor
    /// read before a failure stay in the result.
    fn read_mces(&mut self, boot: &str, cursor: &str) -> KernelRead;
    fn reset_reason(&mut self, boot: &str) -> Result<ResetReason, Error>;
    fn reset_reason_after(&mut self, boot: &str) -> Result<ResetReason, Error>;
    fn saved_pstore(&mut self, boot: &str) -> Result<Option<PstoreRecord>, Error>;
}

pub struct Machine {
    pub clock: Box<dyn Clock>,
    pub smu: Box<dyn Smu>,
    pub host: Box<dyn Host>,
    pub trials: Box<dyn Trials>,
    pub kernel: Box<dyn Kernel>,
}
