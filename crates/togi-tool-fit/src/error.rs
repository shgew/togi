use std::io;
use std::path::PathBuf;

use togi_journal::{Seq, SessionId};
use togi_sim::file::{EncodeError, LoadError};
use togi_sim::{InvalidMachine, ShapeError};
use togi_tool_trialfacts::ReadError;

/// What is wrong with a decisive trial fact for fitting.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Problem {
    #[error("a profile of {0} offsets: the machine needs an even core count from 2")]
    CoreCount(usize),
    #[error("the profile has {found} offsets, the first fact's has {cores}")]
    ProfileSize { found: usize, cores: usize },
    #[error("the trial loaded no core")]
    NoLoadedCore,
    #[error("the trial's intended duration is zero")]
    ZeroDuration,
    #[error("loaded core {core} does not exist on a {cores}-core machine")]
    LoadedCore { core: u8, cores: usize },
}

/// Why the evidence cannot be fitted.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum EvidenceError {
    #[error("extract has no decisive trials")]
    NoTrials,
    #[error("invalid trial fact {session}:{seq}: {problem}")]
    Invalid {
        session: SessionId,
        seq: Seq,
        problem: Problem,
    },
    #[error("mixed BIOS contexts: split the extract before fitting")]
    MixedContexts,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error("invalid fitted machine")]
    Machine(#[from] InvalidMachine),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error("load fitted machine {}", path.display())]
    Load { path: PathBuf, source: LoadError },
    #[error(transparent)]
    Check(#[from] togi_tool_modelcheck::Error),
    #[error("predict a trial")]
    Shape(#[from] ShapeError),
    #[error("{action} {}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("write the report")]
    Report(#[from] io::Error),
    #[error("{} is not valid UTF-8", .0.display())]
    Path(PathBuf),
    #[error("--seal {seal} leaves no held-out session to score ({held_out} held out)")]
    Seal { seal: usize, held_out: usize },
    #[error(
        "{} holds session {session}, recorded after the original research artifact: only --current-extract reads later sessions, and no research campaign does",
        path.display()
    )]
    LaterSession { path: PathBuf, session: String },
    #[error(
        "{} holds sessions of the original research artifact, but its rows hash to {sha256}, not the artifact's {}: write the artifact with `just fit-artifact`",
        path.display(),
        crate::artifact::SHA256
    )]
    NotTheArtifact { path: PathBuf, sha256: String },
    #[error(
        "the original research artifact's sessions in {} hash to {sha256}, not {}: take them from the extract committed at 455704be",
        path.display(),
        crate::artifact::SHA256
    )]
    ArtifactSource { path: PathBuf, sha256: String },
    #[error("{} line {line} is not a forward record", path.display())]
    Records {
        path: PathBuf,
        line: usize,
        source: serde_json::Error,
    },
    #[error("the records do not pair: {0}")]
    Unpaired(String),
    #[error("the shared-voltage fit requires 16 cores: fact {session}:{seq} has {cores}")]
    NeedsSixteenCores {
        session: SessionId,
        seq: Seq,
        cores: usize,
    },
    #[error("shared-voltage in-sample fit requires a BIOS context for real-fact replay")]
    NeedsContext,
    #[error("shared-voltage fitter returned a machine without the 16-core shared-voltage model")]
    NotSharedVoltage,
    #[error("in-sample shared-voltage fit fails the unchanged model check; anchor not written")]
    AnchorFlagged,
    #[error("convert the bootstrap member index")]
    BootstrapIndex(#[source] std::num::TryFromIntError),
    #[error("build the thread pool")]
    Pool(#[from] rayon::ThreadPoolBuildError),
}
