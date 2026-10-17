use crate::object_only;
use crate::{
    BankType, BiosContext, BootId, CancellationToken, Condition, CoreId, CoreInfo, CpuId, Error,
    KernelCursor, LoadSchedule, ObservedOffset, Partial, PerCore, Regime, ResetKind, Signal,
    TargetOffset, TrialConditions, WorkloadId,
};
use jiff::Timestamp;
use serde::Serialize;
use std::time::Duration;

pub trait Clock {
    fn now(&self) -> Timestamp;
    /// Time since boot.
    fn monotonic(&self) -> Duration;
    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error>;
}

/// Proof that every trial workload stopped.
///
/// Only a seam implementation that confirmed every workload stopped may call [`Stopped::confirmed`].
///
/// ```compile_fail
/// let forged = togi_machine::Stopped(());
/// ```
///
/// ```compile_fail
/// fn duplicate(proof: togi_machine::Stopped) -> togi_machine::Stopped {
///     let copy = proof.clone();
///     copy
/// }
/// ```
#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub struct Stopped(());

impl Stopped {
    pub const fn confirmed() -> Self {
        Self(())
    }
}

/// Proof that the SMU interface answered a validation.
///
/// Only a host implementation that validated the SMU may call [`SmuValidated::checked`].
///
/// ```compile_fail
/// let forged = togi_machine::SmuValidated(());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub struct SmuValidated(());

impl SmuValidated {
    pub const fn checked() -> Self {
        Self(())
    }
}

pub trait Smu {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error>;
    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error>;
    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error>;
    /// Writes 0 to every core and nothing else, so it cannot raise an offset's magnitude.
    fn emergency_zero(&mut self, validated: SmuValidated, stopped: &Stopped) -> Result<(), Error>;
}

pub trait Host {
    fn boot_id(&mut self) -> Result<BootId, Error>;
    fn topology(&mut self) -> Result<Vec<CoreInfo>, Error>;
    fn validate_smu(&mut self) -> Result<SmuValidated, Error>;
    fn bios_context(&mut self) -> Result<BiosContext, Error>;
    /// The partial value is the preferred-core values read before the failure, in topology order. An unavailable
    /// ranking is `Err(Partial { value: vec![], error })`.
    fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>>;
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
    pub workload: WorkloadId,
    pub condition: Condition,
    pub cores: Vec<CoreId>,
    pub cpus: Vec<CpuId>,
    pub duration: Duration,
    pub index: usize,
    pub seed: u64,
    /// The offset a readback confirmed on each core as the trial starts; a core whose offset is
    /// not known is absent. The runner keeps togi's own threads off the loaded cores by it.
    pub offsets: PerCore<TargetOffset>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Started {
    pub pid: u32,
    pub scope: String,
    pub cpus: Vec<CpuId>,
    pub argv: Vec<String>,
    pub files: Vec<String>,
    pub instances: Vec<Instance>,
    pub schedule: Option<LoadSchedule>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrialResult {
    pub ran: Duration,
    pub failure: Option<TrialFailure>,
    pub inconclusive: Option<String>,
    pub escaped: Vec<CpuId>,
    pub tctl_max_c: Option<i64>,
    pub stops: u32,
    pub conts: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrialFailure {
    pub signal: Signal,
    pub core: CoreId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instance {
    pub core: CoreId,
    pub cpus: Vec<CpuId>,
    pub pid: u32,
    pub scope: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Sample {
    pub warning: String,
    pub pid: u32,
    pub tid: u32,
    pub cpu: CpuId,
}

pub trait Reporter {
    fn progress(&mut self, detail: &str);
    fn sample(&mut self, sample: Sample);
    fn signal(&mut self, core: CoreId, signal: Signal, detail: &str);
}

pub trait Running {
    fn started(&self) -> Started;
    /// Calls the borrowed reporter only on this thread, and joins the workload before returning.
    /// On `Err(Partial { value, .. })` the value is what the trial accumulated until the failure: for
    /// [`Error::Crashed`] the time run until the crash and the stops and continues counted so far. A refusal that
    /// observed nothing returns `Err(Partial { value: TrialResult::default(), error })`.
    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>>;
    /// Cancels and joins, and proves every workload stopped.
    fn stop(self: Box<Self>) -> Result<Stopped, Error>;
}

/// The outcome of clearing leftovers of earlier runs.
#[derive(Debug)]
pub struct Swept {
    pub detail: String,
    pub stopped: Stopped,
}

pub trait Trials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error>;
    /// Needs the proof that nothing runs. The partial value is `Some` when the failed start's
    /// rollback confirmed nothing runs.
    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>>;
    fn ended(&mut self, id: &str) -> Result<(), Error>;
    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, crate::SampleError>> + 'a>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mce {
    pub cpu: CpuId,
    pub core: Option<CoreId>,
    pub bank: u16,
    pub bank_type: BankType,
    pub corrected: bool,
    pub time: Timestamp,
    pub monotonic: Duration,
    pub lines: Vec<String>,
}

/// What a boot's kernel logged about the reset before it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetReason {
    /// `None` when the boot logged no reason line; `Some(ResetKind::Unknown)` for a line togi does
    /// not recognize.
    pub kind: Option<ResetKind>,
    pub raw: Vec<String>,
    /// Whether the kernel is recent enough (6.16 or later) to log reset reasons.
    pub supported: bool,
}

/// Machine-check events and the cursor after the last log entry read.
#[derive(Debug, Default)]
pub struct KernelRead {
    pub mces: Vec<Mce>,
    pub cursor: KernelCursor,
}

object_only! {
    #[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PstoreRecord {
        pub path: String,
        pub lines: Vec<String>,
    }
}

pub trait Kernel {
    /// An earlier boot missing from the journal is `Error::BootMissing`.
    fn mces(&mut self, boot: &BootId, since: Duration) -> Result<Vec<Mce>, Error>;
    /// Reads after an exact cursor, or the whole boot when `after` is `None`. An unresumable cursor
    /// is `Error::CursorMissing`, never a silently shortened interval. The partial value is the
    /// MCEs read before the failure.
    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<Mce>>>;
    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, Error>;
    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error>;
    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, Error>;
}

pub struct Machine {
    pub clock: Box<dyn Clock>,
    pub smu: Box<dyn Smu>,
    pub host: Box<dyn Host>,
    pub trials: Box<dyn Trials>,
    pub kernel: Box<dyn Kernel>,
}
