use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use togi_backend::{Backend, FileName, Plan};
use togi_machine::CpuId;

use crate::id::TrialId;
use crate::identity::BackendIdentity;
use crate::plan::InstancePlan;

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("resolve the trial directory {}", path.display())]
    Resolve {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("recreate the trial container {}", path.display())]
    Container {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not searchable by uid {uid} gid {gid}", path.display())]
    Inaccessible { path: PathBuf, uid: u32, gid: u32 },
    #[error("create {name} of instance {instance}")]
    Create {
        instance: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("hand {name} of instance {instance} to the backend account")]
    Chown {
        instance: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("plan the backend instance")]
    Backend(#[source] togi_backend::Error),
    #[error("the backend plan has no program")]
    EmptyProgram,
}

/// The trial's root-owned directory, held by descriptor so nothing below is reached by path.
#[derive(Debug)]
pub(crate) struct Container {
    pub(crate) path: PathBuf,
    dir: Arc<OwnedFd>,
}

/// One instance's directory, retained logs and what the backend asked for.
#[derive(Debug)]
pub(crate) struct InstanceFiles {
    pub(crate) path: PathBuf,
    pub(crate) dir: Arc<OwnedFd>,
    pub(crate) stdout: File,
    pub(crate) stderr: File,
    pub(crate) plan: Plan,
    /// Generated input paths relative to the trial directory.
    pub(crate) inputs: Vec<String>,
}

/// Removes any earlier trial of this id without following symlinks, recreates it, fixes its mode
/// (0711 when scoped) by fchmod, and checks the ancestors are searchable by the backend account.
pub(crate) fn recreate(
    root: &Path,
    id: &TrialId,
    identity: Option<BackendIdentity>,
) -> Result<Container, PrepareError> {
    let _ = (root, id, identity);
    unimplemented!()
}

impl Container {
    /// Creates the instance directory (0755), holds both logs `O_EXCL`, then chowns the inputs and
    /// finally the directory to the backend account.
    pub(crate) fn instance(
        &self,
        plan: &InstancePlan,
        backend: &dyn Backend,
        workload: &togi_machine::Workload,
        identity: Option<BackendIdentity>,
    ) -> Result<InstanceFiles, PrepareError> {
        let _ = (plan, backend, workload, identity);
        unimplemented!()
    }

    /// A descriptor for the samples writer, which opens `samples.jsonl` relative to it.
    pub(crate) fn samples_dir(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.dir)
    }
}

pub(crate) fn cpus_list(cpus: &[CpuId]) -> String {
    cpus.iter()
        .map(|cpu| cpu.0.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn _names(_: &FileName) {}
