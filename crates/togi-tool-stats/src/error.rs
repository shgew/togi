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
}
