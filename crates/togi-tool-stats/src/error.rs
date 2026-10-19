use togi_journal::FileError;
use togi_tuner::TopologyError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Journal(#[from] FileError),
    #[error("the journal has no events")]
    Empty,
    #[error("the journal does not open with session.start")]
    NotStarted,
    #[error("the session's cores")]
    Topology(#[from] TopologyError),
    #[error("convert recorded boot uptime")]
    BootDuration(#[source] jiff::Error),
    #[error("reconstruct recorded boot start")]
    BootStart(#[source] jiff::Error),
    #[error("convert recorded conclusion elapsed time")]
    Elapsed(#[source] jiff::Error),
    #[error("convert recorded checking-step count")]
    CheckingSteps(#[source] std::num::TryFromIntError),
    #[error("recorded voltage request {0} cannot be represented as a 4 mV bin")]
    VoltageBin(f64),
    #[error(transparent)]
    Forecast(#[from] togi_tool_forecast::Error),
    #[error("score forecast {path}")]
    Score {
        path: String,
        #[source]
        source: togi_tool_forecast::Error,
    },
    #[error("write chain records {path}")]
    Chains {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("write backoff records {path}")]
    Backoffs {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("fold the journal into the tuner")]
    Fold(#[source] togi_tuner::FoldError),
    #[error("encode the counterfactual records")]
    Records(#[source] serde_json::Error),
    #[error("write counterfactual records {path}")]
    Witnesses {
        path: String,
        #[source]
        source: std::io::Error,
    },
}
