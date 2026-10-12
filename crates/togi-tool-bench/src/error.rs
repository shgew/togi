use std::io;
use std::path::PathBuf;

use togi_sim::{InvalidMachine, file};
use togi_tool_trialfacts::ExtractsError;

use crate::suite::SuiteError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("load suite: {0}")]
    Suite(#[from] SuiteError),
    #[error("load suite: {0}")]
    Gate(#[from] crate::gate::GateError),
    #[error("load scenario {scenario}: {source}")]
    Machine {
        scenario: String,
        #[source]
        source: file::LoadError,
    },
    #[error("load scenario {scenario}: {source}")]
    Replay {
        scenario: String,
        #[source]
        source: ExtractsError,
    },
    #[error("check model {machine}: {message}")]
    ModelCheck { machine: String, message: String },
    #[error("{what} {}: {source}", path.display())]
    ReadFile {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{what} {}: {source}", path.display())]
    WriteFile {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode {what} {} line {line}: {source}", path.display())]
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
    #[error("read the simulator {}: {source}", path.display())]
    NoSimulator {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("start the simulator {}: {reason}", path.display())]
    Unrunnable { path: PathBuf, reason: Unrunnable },
    #[error("build the simulator: {message}")]
    BuildSimulator { message: String },
    #[error("git {command}: {message}")]
    Git { command: String, message: String },
    #[error("{run}: {source}")]
    Run {
        run: String,
        #[source]
        source: Box<RunError>,
    },
    #[error("interrupted")]
    Interrupted,
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
    #[error("create the run directory {}: {source}", path.display())]
    Directory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("start the simulator: {0}")]
    Start(#[source] io::Error),
    #[error("wait for the simulator: {0}")]
    Wait(#[source] io::Error),
    #[error("simulator exceeded the {seconds} s hang safety limit")]
    TimedOut { seconds: u64 },
    #[error("read the run {what}: {source}")]
    Read {
        what: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("read the run journal: {0}")]
    Journal(#[source] togi_journal::FileError),
    #[error("measure the run: {0}")]
    Measure(String),
    #[error("build the run's model: {0}")]
    Model(#[source] InvalidMachine),
    #[error("interrupted")]
    Interrupted,
}
