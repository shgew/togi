use togi_journal::{Seq, SessionId};
use togi_machine::ResetKind;
use togi_sim::{InvalidMachine, ShapeError};
use togi_tool_trialfacts::{ExtractsError, ReadError};

/// Why a model check could not run. A check that runs and finds a disagreement is a
/// [`Check`](crate::Check) with status [`Flagged`](crate::Status::Flagged), not an error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid machine: {0}")]
    Machine(#[from] InvalidMachine),
    #[error(transparent)]
    Extract(ExtractsError),
    #[error("read the machine's facts extract: {0}")]
    ReadExtract(#[source] ReadError),
    /// A `thermal_trip` or `power_loss` reset can turn a simulated crash into an inconclusive trial, which
    /// the extract does not hold, so the machine's failure probability would be compared with the wrong
    /// denominator.
    #[error("model check requires decisive failures: reset {0} has positive weight")]
    IndecisiveReset(ResetKind),
    #[error("invalid trial fact {session}:{seq}: {problem}")]
    InvalidFact {
        session: SessionId,
        seq: Seq,
        problem: FactProblem,
    },
    /// A predictor whose machine differs in shape from the one the checker was built for.
    #[error("predict a checked trial: {0}")]
    Shape(#[from] ShapeError),
}

impl From<ExtractsError> for Error {
    fn from(error: ExtractsError) -> Self {
        match error {
            ExtractsError::Read(error) => Self::ReadExtract(error),
            other @ (ExtractsError::NoFacts
            | ExtractsError::NoContext
            | ExtractsError::Invalid(_)) => Self::Extract(other),
        }
    }
}

/// What is wrong with a trial fact for the machine it is checked against.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FactProblem {
    #[error("the profile has {found} offsets for {cores} cores")]
    ProfileSize { found: usize, cores: usize },
    #[error("the trial loaded no core")]
    NoLoadedCore,
    #[error("the trial's intended duration is zero")]
    ZeroDuration,
    #[error("loaded core {core} does not exist on a {cores}-core machine")]
    LoadedCore { core: u8, cores: usize },
}
