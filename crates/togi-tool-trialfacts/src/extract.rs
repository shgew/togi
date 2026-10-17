use std::convert::Infallible;
use std::path::Path;

use togi_facts::{Decisive, Fact, FactsError, Observation, SessionFacts};
use togi_journal::{
    Build, LookupError, Operation, Payload, Recording, Refusal, SCHEMA, Seq, SessionId, Skipped,
    StateDir, Verdict,
};
use togi_machine::{CoreSet, PerCore};

use crate::record::{BuildStamp, Class, InvalidProfile, Phase, Profile, TrialFact};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("{}", togi_machine::error_chain(.0))]
    Journals(#[from] LookupError),
    #[error("lock the state directory: {}", togi_machine::error_chain(.0))]
    Lock(#[from] togi_journal::LockError),
    #[error("session {session}: {source}")]
    Facts {
        session: SessionId,
        #[source]
        source: FactsError<Infallible>,
    },
    #[error("session {session}: {source}")]
    Incompatible {
        session: SessionId,
        #[source]
        source: Refusal,
    },
    #[error("session {session} event {seq}: {source}")]
    Profile {
        session: SessionId,
        seq: Seq,
        #[source]
        source: InvalidProfile,
    },
    #[error("no facts to extract")]
    NoFacts,
}

/// The facts of a state directory and what reading its journals passed over to get them.
#[derive(Debug)]
pub struct Extracted {
    pub facts: Vec<TrialFact>,
    /// Each session whose journal held events or fields this build does not know, in session order, with what
    /// the read skipped or ignored; empty when every journal was read whole.
    pub skipped: Vec<(SessionId, Skipped)>,
}

/// The facts of every session in the state directory, archived ones oldest first and then the live one, each
/// session's in journal order. The journals are history, so they are read leniently: an event of a kind this
/// build does not know is skipped and an unknown field ignored, and [`Extracted::skipped`] says so. Extract from
/// a copy of the directory, never the live state: reading takes the directory's writer lock.
pub fn extract(dir: &StateDir) -> Result<Extracted, ExtractError> {
    from_recordings(dir.recordings_lenient()?)
}

/// [`extract`] on the state directory at `path`.
pub fn extract_dir(path: impl AsRef<Path>) -> Result<Extracted, ExtractError> {
    extract(&StateDir::lock(path)?)
}

/// The facts of journals read one by one, in session order. A journal without events is skipped; one whose
/// schema this build cannot read as history is refused before extraction; no facts at all is an error, since
/// an empty extract is no evidence. Each recording's [`skipped`](Recording::skipped) is reported under its
/// session.
pub fn from_recordings(
    recordings: impl IntoIterator<Item = Recording>,
) -> Result<Extracted, ExtractError> {
    let mut sessions = Vec::new();
    let mut skipped = Vec::new();
    for recording in recordings {
        let passed_over = recording.skipped().clone();
        let events = recording.into_events();
        let Some(Payload::SessionStart(start)) = events.first().map(|event| &event.payload) else {
            continue;
        };
        let session = start.session;
        let reader = Build {
            schema: SCHEMA,
            ..start.build.clone()
        };
        if let Verdict::Refuse(source) = reader.verdict(&start.build, Operation::History) {
            return Err(ExtractError::Incompatible { session, source });
        }
        let facts = togi_facts::extract(session, events.into_iter().map(Ok::<_, Infallible>))
            .map_err(|source| ExtractError::Facts { session, source })?;
        sessions.push(facts);
        if !passed_over.is_empty() {
            skipped.push((session, passed_over));
        }
    }
    sessions.sort_by_key(|session| session.id);
    skipped.sort_by_key(|(session, _)| *session);
    let mut records = Vec::new();
    for session in &sessions {
        for fact in &session.facts {
            records.push(record(session, fact)?);
        }
    }
    if records.is_empty() {
        return Err(ExtractError::NoFacts);
    }
    Ok(Extracted {
        facts: records,
        skipped,
    })
}

fn record(session: &SessionFacts, fact: &Fact) -> Result<TrialFact, ExtractError> {
    let profile =
        Profile::from_per_core(fact.profile()).map_err(|source| ExtractError::Profile {
            session: fact.source.session,
            seq: fact.source.seq,
            source,
        })?;
    let (class, condition, phase, outcome, duration_s, telemetry) = match &fact.observation {
        Observation::Trial {
            class,
            condition,
            activity,
            outcome,
            duration_s,
            telemetry,
            ..
        } => (
            Class::Trial(*class),
            *condition,
            activity.map(Phase::from),
            *outcome,
            *duration_s,
            telemetry.as_ref(),
        ),
        Observation::IdleFailure {
            loaded,
            condition,
            signal,
            core,
            ..
        } => (
            Class::Idle { loaded: *loaded },
            *condition,
            None,
            Decisive::Failure {
                signal: *signal,
                core: *core,
                stalled: None,
            },
            0,
            None,
        ),
    };
    Ok(TrialFact {
        session: fact.source.session,
        build: BuildStamp::from(&fact.source.build),
        context: session.context.clone(),
        seq: fact.source.seq,
        trial: fact.source.trial.clone(),
        class,
        condition,
        phase,
        record_only: false,
        profile,
        outcome,
        duration_s,
        voltage_requests_v: telemetry.map_or_else(PerCore::default, |t| t.voltage_requests_v),
        top_requesters: telemetry.map_or_else(CoreSet::default, |t| t.top_requesters),
        ccd_mhz: telemetry.map(|t| t.ccd_mhz.clone()).unwrap_or_default(),
    })
}
