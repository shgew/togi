use togi_journal::{Seq, TrialId};
use togi_machine::{CoreId, Regime, WorkloadId};

use crate::{ConstraintError, ProfileError, RankingError, TopologyError, TopologyMismatch};

/// A journal the tuner cannot have written: the event contradicts the state it folds into. The
/// state is unchanged when `fold` returns one.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FoldError {
    #[error("event {seq} does not follow event {last}")]
    OutOfOrder { last: Seq, seq: Seq },
    #[error("session.start has not been folded")]
    NotStarted,
    #[error("session.start folded twice")]
    DuplicateStart,
    #[error("topology: {0}")]
    Topology(#[from] TopologyError),
    #[error("core {0} is outside the topology")]
    UnknownCore(CoreId),
    #[error("profile: {0}")]
    Profile(#[from] ProfileError),
    #[error("{0}")]
    Mismatch(#[from] TopologyMismatch),
    #[error("an earlier event could not be folded")]
    Poisoned,
    #[error("ranking: {0}")]
    Ranking(#[from] RankingError),
    #[error("constraint: {0}")]
    Constraint(#[from] ConstraintError),
    #[error("trial {0} is not the open trial")]
    NoOpenTrial(TrialId),
    #[error("trial {0} has no failed end awaiting attribution")]
    NotAwaiting(TrialId),
    #[error("trial {0} is not one the tuner schedules")]
    UnscheduledIntent(TrialId),
    #[error("trial {trial} began while trial {open} is still open or awaiting attribution")]
    OverlappingIntent { trial: TrialId, open: TrialId },
    #[error("failure #{0} is unknown")]
    UnknownFailure(Seq),
    #[error("workload {workload} is not an {regime} workload")]
    WorkloadRegime {
        regime: Regime,
        workload: WorkloadId,
    },
    #[error("a candidate solo limit of core {0} needs its R1 and R2 workloads")]
    BadCandidate(CoreId),
    #[error("trial {0} loads no core")]
    NoLoad(TrialId),
    #[error("a trial duration is zero")]
    ZeroDuration,
    #[error("a failed trial carries no signal")]
    MissingSignal,
    #[error("a failure belongs to no trial and is not an idle crash")]
    NoFailureClass,
    #[error("a failure records no profile and no trial or checking profile supplies one")]
    NoProfile,
    #[error("a checking cycle passed before any profile.change")]
    NoCheckingProfile,
    #[error("CCD {} is outside the topology", .0.0)]
    UnknownCcd(togi_machine::CcdId),
    #[error("hunt {0} has no candidate to narrow")]
    NoCandidates(std::num::NonZeroU32),
    #[error("config.loaded records a configuration this build refuses: {0}")]
    Config(String),
}
