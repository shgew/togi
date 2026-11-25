use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{what} {}", path.display())]
    WriteFile {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
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
}

/// Why the simulator did not build.
#[derive(Debug, thiserror::Error)]
pub enum BuildFailure {
    #[error("run cargo")]
    Spawn(#[source] io::Error),
    #[error("cargo build")]
    Run(#[source] RunError),
    #[error("cargo build exit status: {0}")]
    Exit(i32),
    #[error("keep cargo's messages")]
    Messages(#[source] io::Error),
    #[error("cargo reported no togi-tool-sim executable")]
    NoExecutable,
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

/// What stopped an attempt to build or run a simulator.
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
    #[error("interrupted")]
    Interrupted,
}

/// A scripted child did not complete its protocol.
#[derive(Debug, thiserror::Error)]
pub enum ScriptedError {
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("scripted simulator exited with status {exit}")]
    Exit { exit: i32 },
    #[error(
        "scripted-contract printed {answer:?}; expected version {} followed by a newline",
        togi_tool_sim::scripted::CONTRACT_VERSION
    )]
    Version { answer: String },
}
