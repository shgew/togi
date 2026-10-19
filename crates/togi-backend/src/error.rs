use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{name} binary {} does not exist", .path.display())]
    Missing {
        name: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("stat {name} binary {}", .path.display())]
    Stat {
        name: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("{name} binary {} is not an executable regular file", .path.display())]
    NotExecutable { name: &'static str, path: PathBuf },
    #[error("{name} binaries directory {} does not exist", .root.display())]
    BinariesMissing {
        name: &'static str,
        root: PathBuf,
        source: io::Error,
    },
    #[error("read {name} binaries directory {}", .root.display())]
    ReadBinaries {
        name: &'static str,
        root: PathBuf,
        source: io::Error,
    },
    #[error("no {name} binaries in {}", .root.display())]
    NoBinaries { name: &'static str, root: PathBuf },
    #[error("no Zen 5 binary 24-ZN5 in {}", .root.display())]
    NoZen5 { root: PathBuf },
    #[error("{backend}: no configuration for workload {workload}")]
    Unsupported {
        backend: togi_machine::Backend,
        workload: togi_machine::WorkloadId,
    },
    #[error("{backend}: workload {workload} needs at least one logical CPU")]
    NoCpus {
        backend: togi_machine::Backend,
        workload: togi_machine::WorkloadId,
    },
    #[error("compile {name} output pattern")]
    Pattern {
        name: &'static str,
        source: regex::Error,
    },
}

impl Error {
    /// Whether the backend is simply not installed, as opposed to unreadable or misconfigured.
    pub const fn is_missing(&self) -> bool {
        matches!(
            self,
            Self::Missing { .. }
                | Self::BinariesMissing { .. }
                | Self::NoBinaries { .. }
                | Self::NoZen5 { .. }
        )
    }
}

impl From<Error> for togi_machine::Error {
    fn from(error: Error) -> Self {
        if error.is_missing() {
            Self::BackendMissing
        } else {
            Self::Other(Box::new(error))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("parse backend affinity CPU {value}")]
pub struct InvalidAffinityCpu {
    pub value: String,
    #[source]
    pub source: std::num::ParseIntError,
}
