//! Shared test doubles.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use togi_backend::{Backend, Error, FileName, Input, Line, Plan, Ready};
use togi_machine::{CpuId, Workload};

/// A current-thread runtime with timers, but no reactor or OS descriptors.
pub(crate) fn runtime(paused: bool) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(paused)
        .build()
        .expect("build the timer-only test runtime")
}

/// Keeps parameterised test attributes while running an async fixture without an I/O reactor.
macro_rules! test {
    ($(#[$attribute:meta])* async fn $name:ident($($arguments:tt)*) $(-> $result:ty)? $body:block) => {
        $(#[$attribute])*
        #[test]
        fn $name($($arguments)*) $(-> $result)? {
            crate::testing::runtime(true).block_on(async move $body)
        }
    };
}

pub(crate) use test;

/// A backend that classifies `COMPUTE ERROR` anywhere as a computation error, `PIN FAILED` as
/// a setup error, `AFFINITY:<n>` as an affinity error and `ok` as a progress line.
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
    fn configuration(&self, workload: &Workload) -> Result<Vec<u8>, Error> {
        let plan = self.plan(
            workload,
            Path::new(""),
            &togi_backend::numbered_cpus(workload.threads),
        )?;
        Ok(togi_backend::configuration_bytes(&plan.args, &plan.inputs))
    }

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
            match cpu.parse::<u16>() {
                Ok(cpu) => Line::AffinityError(Some(CpuId::from(cpu))),
                Err(source) if source.kind() == &std::num::IntErrorKind::PosOverflow => {
                    Line::InvalidAffinity(togi_backend::InvalidAffinityCpu {
                        value: cpu.to_owned(),
                        source,
                    })
                }
                Err(_) => Line::AffinityError(None),
            }
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
