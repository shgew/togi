use std::io;
use std::path::PathBuf;

use togi_journal::{FileError, Refusal, Seq, SessionId};
use togi_machine::CoreSet;

/// Why a forecast could not be read, replayed, summarized or scored.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read journal {}: {source}", path.display())]
    ReadJournal {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("list archive {}: {source}", path.display())]
    ListArchive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Journal(#[from] FileError),
    #[error("encode event {seq}: {source}")]
    Encode {
        seq: Seq,
        #[source]
        source: serde_json::Error,
    },
    #[error("the events do not open with session.start")]
    NotStarted,
    #[error("live journal in {} has no events", dir.display())]
    NoEvents { dir: PathBuf },
    #[error("anchor not found: no journal holds session {session}")]
    NoSession { session: SessionId },
    #[error("anchor not found: session {session} has no event {seq}")]
    NoEvent { session: SessionId, seq: Seq },
    #[error(
        "anchor changed: session {session} through event {seq} differs from the forecast's copy"
    )]
    ChangedAnchor { session: SessionId, seq: Seq },
    #[error("session {session} cannot be replayed: {refusal}")]
    NotReplayable {
        session: SessionId,
        refusal: Refusal,
    },
    #[error("session {session}: {source}")]
    JournalFold {
        session: SessionId,
        #[source]
        source: togi_journal::FoldError,
    },
    #[error("session {session}: {source}")]
    TunerFold {
        session: SessionId,
        #[source]
        source: togi_tuner::FoldError,
    },
    #[error("forecast has no runs")]
    NoRuns,
    #[error("run {machine} seed {seed} has cores {run}, others {first}", run = cores(*.run), first = cores(*.first))]
    RunCores {
        machine: String,
        seed: u64,
        run: CoreSet,
        first: CoreSet,
    },
    #[error("run has cores {run}, forecast {forecast}", run = cores(*.run), forecast = cores(*.forecast))]
    ScoreCores { run: CoreSet, forecast: CoreSet },
    #[error("read forecast {}: {source}", path.display())]
    ReadRecord {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode forecast {}: {source}", path.display())]
    DecodeRecord {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("forecast {} has no runs", path.display())]
    EmptyRecord { path: PathBuf },
    #[error("write forecast: {0}")]
    WriteRecord(#[source] io::Error),
}

fn cores(set: CoreSet) -> String {
    let ids: Vec<String> = set.iter().map(|core| core.to_string()).collect();
    format!("[{}]", ids.join(", "))
}
