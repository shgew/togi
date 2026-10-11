//! Shared test doubles.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use togi_backend::{Backend, Error, FileName, Input, Line, Plan, Ready};
use togi_machine::{CpuId, Workload};

/// A backend with the classifier the Go tests used: `COMPUTE ERROR` anywhere is a computation
/// error, `PIN FAILED` a setup error, `AFFINITY:<n>` an affinity error, `ok` a progress line.
#[derive(Clone, Debug)]
pub(crate) struct TestBackend {
    pub(crate) kind: togi_machine::Backend,
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) inputs: Vec<(&'static str, &'static str)>,
    pub(crate) watched: Vec<&'static str>,
    pub(crate) fail_plan: bool,
    pub(crate) panic_on: Option<&'static str>,
}

impl TestBackend {
    pub(crate) fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            kind: togi_machine::Backend::Mprime,
            program: program.into(),
            args: Vec::new(),
            inputs: Vec::new(),
            watched: Vec::new(),
            fail_plan: false,
            panic_on: None,
        }
    }
}

impl Backend for TestBackend {
    fn kind(&self) -> togi_machine::Backend {
        self.kind
    }

    fn check(&self) -> Result<Ready, Error> {
        Ok(Ready {
            detail: "test backend".to_owned(),
        })
    }

    fn plan(&self, workload: &Workload, _dir: &Path, _cpus: &[CpuId]) -> Result<Plan, Error> {
        if self.fail_plan {
            return Err(Error::Unsupported {
                backend: self.kind,
                workload: workload.id(),
            });
        }
        Ok(Plan {
            program: self.program.clone(),
            args: self.args.clone(),
            inputs: self
                .inputs
                .iter()
                .map(|(name, contents)| Input {
                    name: name
                        .parse::<FileName>()
                        .unwrap_or_else(|_| FileName::from_static("input")),
                    contents: contents.as_bytes().to_vec(),
                })
                .collect(),
            watched: self
                .watched
                .iter()
                .map(|name| {
                    name.parse::<FileName>()
                        .unwrap_or_else(|_| FileName::from_static("results.txt"))
                })
                .collect(),
        })
    }

    fn classify(&self, line: &str) -> Line {
        if let Some(trigger) = self.panic_on {
            assert!(!line.contains(trigger), "the classifier met {trigger}");
        }
        if line.contains("COMPUTE ERROR") {
            Line::ComputationError
        } else if line.starts_with("PIN FAILED") {
            Line::SetupError
        } else if let Some(cpu) = line.strip_prefix("AFFINITY:") {
            Line::AffinityError(cpu.parse().ok().map(CpuId))
        } else if line.starts_with("ok") {
            Line::Progress(line.to_owned())
        } else {
            Line::Other
        }
    }
}

/// A temporary directory only its owner can write, whatever the umask: the trials root refuses a
/// parent that others can write.
pub(crate) fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let dir =
        tempfile::tempdir().unwrap_or_else(|error| panic!("create a temporary directory: {error}"));
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|error| panic!("set the temporary directory's mode: {error}"));
    dir
}
