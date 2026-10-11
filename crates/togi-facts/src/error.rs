use togi_journal::{Seq, SessionId, TrialId};

#[derive(Debug, thiserror::Error)]
pub enum FactsError<E> {
    #[error(transparent)]
    Read(E),
    #[error("the stream does not begin with session.start")]
    NotStarted,
    #[error("event {seq}: a second session.start")]
    DuplicateStart { seq: Seq },
    #[error("session.start records {recorded}, not the extracted session {expected}")]
    SessionMismatch {
        expected: SessionId,
        recorded: SessionId,
    },
    #[error("session.start records no cores")]
    EmptyTopology,
    #[error("event {seq}: config.loaded changes the session's {field}, fixed by session.start")]
    StampChanged { seq: Seq, field: &'static str },
    #[error("event {seq}: trial {trial} already has an intent")]
    DuplicateIntent { trial: TrialId, seq: Seq },
    #[error("event {seq}: trial {trial} already ended with a decisive outcome")]
    DuplicateEnd { trial: TrialId, seq: Seq },
    #[error("event {seq}: {field} is required of a decisive fact")]
    Incomplete { seq: Seq, field: &'static str },
    #[error("event {seq}: a carried trial must be a pass or a failure")]
    NotDecisive { seq: Seq },
    #[error("event {seq}: a carried failure must be an unattributed failure outside any trial")]
    NotIdle { seq: Seq },
}
