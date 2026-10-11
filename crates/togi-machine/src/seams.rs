use crate::{
    BankType, BiosContext, CancellationToken, Condition, CoreId, CoreInfo, CpuId, NanoDuration,
    ObservedOffset, Regime, ResetKind, Signal, TargetOffset, TrialConditions, Workload,
};
use jiff::Timestamp;
use serde::Serialize;
use std::error::Error as StdError;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ErrorKind {
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
    #[error("context canceled")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Kind(#[from] ErrorKind),
    #[error("{detail}: {source}")]
    Context { detail: String, source: Box<Self> },
    #[error("{}", joined_text(.0))]
    Joined(Vec<Self>),
    #[error(transparent)]
    Other(Box<dyn StdError + Send + Sync>),
}

fn joined_text(errors: &[Error]) -> String {
    use fmt::Write;
    let mut result = String::new();
    for (index, error) in errors.iter().enumerate() {
        if index != 0 {
            result.push('\n');
        }
        let _ = write!(result, "{error}");
    }
    result
}

impl Error {
    pub fn contains(&self, kind: ErrorKind) -> bool {
        match self {
            Self::Kind(found) => *found == kind,
            Self::Context { source, .. } => source.contains(kind),
            Self::Joined(errors) => errors.iter().any(|error| error.contains(kind)),
            Self::Other(source) => {
                let mut error: &(dyn StdError + 'static) = source.as_ref();
                loop {
                    if error.downcast_ref::<ErrorKind>() == Some(&kind) {
                        return true;
                    }
                    if let Some(error) = error.downcast_ref::<Self>() {
                        return error.contains(kind);
                    }
                    if let Some(io) = error.downcast_ref::<std::io::Error>() {
                        if let Some(inner) = io.get_ref() {
                            error = inner;
                            continue;
                        }
                    }
                    match error.source() {
                        Some(source) => error = source,
                        None => return false,
                    }
                }
            }
        }
    }
}

pub type SeamResult<T> = Result<T, Error>;

/// A value together with the error that ended the call, like Go's `(T, error)` where the caller
/// reads the value even when the error is set.
#[derive(Debug)]
pub struct SeamOutcome<T> {
    pub value: T,
    pub error: Option<Error>,
}

impl<T> SeamOutcome<T> {
    pub const fn ok(value: T) -> Self {
        Self { value, error: None }
    }

    pub const fn failed(value: T, error: Error) -> Self {
        Self {
            value,
            error: Some(error),
        }
    }

    /// Drops the value when the call failed.
    pub fn into_result(self) -> SeamResult<T> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.value),
        }
    }
}

pub trait Clock {
    fn now(&self) -> Timestamp;
    fn monotonic(&self) -> NanoDuration;
    fn sleep(&mut self, cancel: &CancellationToken, duration: NanoDuration) -> SeamResult<()>;
}

pub trait Smu {
    fn offset(&mut self, core: CoreId) -> SeamResult<ObservedOffset>;
    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> SeamResult<()>;
    fn set_all_offsets(&mut self, offset: TargetOffset) -> SeamResult<()>;
}

pub trait Host {
    fn boot_id(&mut self) -> SeamResult<String>;
    fn topology(&mut self) -> SeamResult<Vec<CoreInfo>>;
    fn validate_smu(&mut self) -> SeamResult<()>;
    fn bios_context(&mut self) -> SeamResult<BiosContext>;
    /// `None` is a nil ranking; the session journals the values it holds even when the read failed.
    fn ranking(&mut self) -> SeamOutcome<Option<Vec<CoreRank>>>;
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
    pub cores: Option<Vec<CoreId>>,
    pub cpus: Option<Vec<CpuId>>,
    pub duration: NanoDuration,
    pub index: usize,
    pub seed: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Started {
    pub pid: i64,
    pub scope: String,
    pub cpus: Option<Vec<CpuId>>,
    pub argv: Option<Vec<String>>,
    pub files: Option<Vec<String>>,
    pub instances: Option<Vec<Instance>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialResult {
    pub ran: NanoDuration,
    pub signal: Signal,
    pub core: CoreId,
    pub inconclusive: String,
    pub escaped: Option<Vec<CpuId>>,
    pub tctl_max_c: Option<i64>,
    pub stops: i64,
    pub conts: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instance {
    pub core: CoreId,
    pub cpus: Option<Vec<CpuId>>,
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

pub trait Running {
    fn started(&self) -> Started;
    /// Calls the borrowed reporter only on this thread, and joins the workload before returning.
    /// The accumulated result comes back even when the wait also failed.
    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> SeamOutcome<TrialResult>;
    /// Cancels and joins; repeated calls return the first teardown result.
    fn stop(&mut self) -> SeamResult<()>;
}

pub trait Trials {
    fn sweep(&mut self, cancel: &CancellationToken) -> SeamResult<String>;
    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
    ) -> SeamResult<Box<dyn Running>>;
    fn ended(&mut self, id: &str) -> SeamResult<()>;
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
    pub monotonic: NanoDuration,
    pub lines: Option<Vec<crate::HostText>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetReason {
    pub kind: ResetKind,
    pub raw: crate::HostText,
    pub supported: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KernelRead {
    pub mces: Option<Vec<Mce>>,
    pub cursor: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct PstoreRecord {
    pub path: crate::HostText,
    pub lines: Option<Vec<crate::HostText>>,
}

/// `mces` and `read_mces` return `SeamOutcome` because the session uses the evidence read before a
/// failure (`internal/session/kernel.go`, `readMCEs` on a missing earlier boot). The reset-reason and
/// pstore reads stay `SeamResult`: their callers drop the value whenever the error is not a missing boot.
pub trait Kernel {
    /// An earlier boot missing from the journal returns an empty read with `ErrorKind::BootMissing`.
    fn mces(&mut self, boot: &str, since: NanoDuration) -> SeamOutcome<Vec<Mce>>;
    /// Keeps the validated MCEs and last cursor read before a failure; an unresumable cursor is `ErrorKind::CursorMissing`.
    fn read_mces(&mut self, boot: &str, cursor: &str) -> SeamOutcome<KernelRead>;
    fn reset_reason(&mut self, boot: &str) -> SeamResult<ResetReason>;
    fn reset_reason_after(&mut self, boot: &str) -> SeamResult<ResetReason>;
    fn saved_pstore(&mut self, boot: &str) -> SeamResult<Option<PstoreRecord>>;
}

pub struct Machine {
    pub clock: Box<dyn Clock>,
    pub smu: Box<dyn Smu>,
    pub host: Box<dyn Host>,
    pub trials: Box<dyn Trials>,
    pub kernel: Box<dyn Kernel>,
}
