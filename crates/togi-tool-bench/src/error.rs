use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

use togi_sim::{InvalidMachine, file};
use togi_tool_trialfacts::ExtractsError;

use crate::suite::SuiteError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("load suite")]
    Suite(#[from] SuiteError),
    #[error("load suite")]
    Gate(#[from] crate::gate::GateError),
    #[error("load scenario {scenario}")]
    Machine {
        scenario: String,
        #[source]
        source: file::LoadError,
    },
    #[error("load scenario {scenario}")]
    Replay {
        scenario: String,
        #[source]
        source: ExtractsError,
    },
    #[error("check model {machine}")]
    ModelCheck {
        machine: String,
        #[source]
        source: ModelCheckError,
    },
    #[error("{what} {}", path.display())]
    ReadFile {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{what} {}", path.display())]
    WriteFile {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode {what} {} line {line}", path.display())]
    Decode {
        what: &'static str,
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("duplicate {what} run {scenario}/{seed}")]
    DuplicateRun {
        what: &'static str,
        scenario: String,
        seed: u64,
    },
    #[error("read the simulator {}", path.display())]
    NoSimulator {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("start the simulator {}", path.display())]
    Unrunnable {
        path: PathBuf,
        #[source]
        reason: Unrunnable,
    },
    #[error("build the simulator")]
    BuildSimulator(#[source] BuildFailure),
    #[error("git {command}")]
    Git {
        command: String,
        #[source]
        source: GitError,
    },
    #[error("{run}")]
    Run {
        run: String,
        #[source]
        source: Box<RunError>,
    },
    #[error("interrupted")]
    Interrupted,
}

/// Why a model check of a replayed machine did not run.
#[derive(Debug, thiserror::Error)]
pub enum ModelCheckError {
    #[error("build the machine's model")]
    Model(#[source] InvalidMachine),
    #[error(transparent)]
    Extract(ExtractsError),
    #[error(transparent)]
    Check(togi_tool_modelcheck::Error),
}

/// Why the simulator did not build.
#[derive(Debug, thiserror::Error)]
pub enum BuildFailure {
    #[error("run cargo")]
    Spawn(#[source] io::Error),
    #[error("cargo build")]
    Run(#[source] RunError),
    #[error("cargo build {0}")]
    Status(ExitStatus),
    #[error("cargo build exit status: {0}")]
    Exit(i32),
    #[error("keep cargo's messages")]
    Messages(#[source] io::Error),
    #[error("cargo reported no togi-tool-sim executable")]
    NoExecutable,
}

/// Why a git command did not answer.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error(transparent)]
    Spawn(io::Error),
    /// What git printed on stderr.
    #[error("{0}")]
    Failed(String),
    #[error("resolve the output path")]
    Resolve(#[source] io::Error),
}

/// Why a simulator file is not something the bench starts.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unrunnable {
    #[error("not executable")]
    NotExecutable,
    #[error("not an executable for this platform or a #! script")]
    NotAProgram,
    #[error("#! line does not end within 512 bytes")]
    LongShebang,
    #[error("#! names no interpreter")]
    NoInterpreter,
    #[error("read the interpreter {}: {kind}", interpreter.display())]
    UnreadableInterpreter {
        interpreter: PathBuf,
        kind: io::ErrorKind,
    },
    #[error("interpreter {} is not executable", interpreter.display())]
    InterpreterNotExecutable { interpreter: PathBuf },
    #[error("interpreter {} is not an executable for this platform", interpreter.display())]
    InterpreterNotAProgram { interpreter: PathBuf },
}

/// What stopped one run of the suite. These are harness errors: a session's own outcome, a dead end or a boot
/// cap included, is a [`Status`](crate::Status) of its record.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("create the run directory {}", path.display())]
    Directory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("start the simulator")]
    Start(#[source] io::Error),
    #[error("wait for the simulator")]
    Wait(#[source] io::Error),
    #[error("simulator exceeded the {seconds} s hang safety limit")]
    TimedOut { seconds: u64 },
    #[error("read the run {what}")]
    Read {
        what: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("read the run journal")]
    Journal(#[source] togi_journal::FileError),
    #[error("measure the run")]
    Measure(#[source] MeasureError),
    #[error("build the run's model")]
    Model(#[source] InvalidMachine),
    #[error("interrupted")]
    Interrupted,
}

/// Why a finished run's journal could not be measured.
#[derive(Debug, thiserror::Error)]
pub enum MeasureError {
    #[error(transparent)]
    Shape(togi_sim::ShapeError),
    #[error("replay the journal")]
    Replay(#[source] togi_journal::FoldError),
    #[error("restore the folded state")]
    Folded(#[source] togi_session::FoldError),
    #[error("extract the trial facts")]
    Facts(#[source] togi_facts::FactsError<std::convert::Infallible>),
}
