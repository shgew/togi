use crate::identity::CpuSummary;
use crate::mailbox::MailboxError;
use crate::slots::SlotError;
use std::fmt;
use std::io;
use std::num::ParseIntError;
use std::path::PathBuf;
use std::sync::Arc;
use togi_machine::CoreId;

/// What a driver call was doing when it was refused or failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Validate,
    MapSlots,
    ReadOffset(CoreId),
    SetOffset(CoreId),
    SetAllOffsets,
    EmergencyZero,
    ReadBiosContext,
    ReadPmTable,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validate => f.write_str("validate the SMU interface"),
            Self::MapSlots => f.write_str("map core slots"),
            Self::ReadOffset(core) => write!(f, "read core {core:02} offset"),
            Self::SetOffset(core) => write!(f, "set core {core:02} offset"),
            Self::SetAllOffsets => f.write_str("set all offsets"),
            Self::EmergencyZero => f.write_str("zero all offsets"),
            Self::ReadBiosContext => f.write_str("read BIOS context"),
            Self::ReadPmTable => f.write_str("read the PM table"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the SMU driver needs Linux")]
    Unsupported,
    #[error("list {}", path.display())]
    ListCpus {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read {}", path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parse {}: {text:?} is not a decimal integer", path.display())]
    ParseInt {
        path: PathBuf,
        text: String,
        #[source]
        source: ParseIntError,
    },
    #[error("CPU directory {name:?} names a CPU beyond 65535")]
    CpuNumber { name: String },
    #[error("no online CPU cores in {}", path.display())]
    NoOnlineCpus { path: PathBuf },
    #[error("parse cpu family {family:?} and model {model:?}")]
    ParseCpuInfo { family: String, model: String },
    #[error("{summary}: not Granite Ridge desktop (family 0x1A, model 0x40-0x4F)")]
    UnsupportedCpu { summary: CpuSummary },
    #[error("/sys/kernel/ryzen_smu_drv not found: ryzen_smu is not loaded")]
    DriverNotLoaded,
    #[error("codename {found}, want 23 (Granite Ridge)")]
    Codename { found: String },
    #[error("{operation}: CPU and driver not validated")]
    NotValidated { operation: Operation },
    #[error("{operation}: opened for identity checks only")]
    IdentityOnly { operation: Operation },
    #[error("{operation}: per-core access refused")]
    SlotMapping {
        operation: Operation,
        #[source]
        source: Arc<SlotError>,
    },
    #[error(transparent)]
    Mapping(Arc<SlotError>),
    #[error("core {core:02} not found in topology")]
    UnknownCore { core: CoreId },
    #[error("core ID {core_id} on CCD{ccd} outside supported range 0-15")]
    CoreOutOfRange { core_id: i64, ccd: u8 },
    #[error(transparent)]
    Topology(#[from] SlotError),
    #[error("{operation}")]
    Mailbox {
        operation: Operation,
        #[source]
        source: MailboxError,
    },
    #[error("start a worker thread")]
    WorkerSpawn(#[source] io::Error),
    #[error("the driver lost its mailbox when its worker thread could not start")]
    MailboxLost,
    #[error("core {core:02}")]
    ReadBack {
        core: CoreId,
        #[source]
        source: Box<Self>,
    },
}

impl From<Error> for togi_machine::Error {
    fn from(error: Error) -> Self {
        Self::Other(Box::new(error))
    }
}

/// The error and every cause below it, joined as `outer: inner`.
pub(crate) fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}
