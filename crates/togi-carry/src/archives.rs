#![expect(
    clippy::trait_duplication_in_bounds,
    reason = "thiserror repeats CarryError's error bound in generated impls outside the enum's lint scope"
)]

use std::error::Error;

use togi_facts::FactsError;
use togi_journal::{Event, SessionId};

/// Where carry reads archived sessions from. The session implements it over the journal's archive listing; carry
/// calls [`Archives::events`] at most once per source and never for one behind the reset boundary.
pub trait Archives {
    type Error: Error + 'static;
    type Events: IntoIterator<Item = Result<Event, Self::Error>>;

    /// The events of an archived session, or `None` when no such archive exists.
    fn events(&mut self, session: SessionId) -> Result<Option<Self::Events>, Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum CarryError<E: Error + 'static> {
    #[error("open archived session {session}")]
    Open {
        session: SessionId,
        #[source]
        source: E,
    },
    #[error("read archived session {session}")]
    Read {
        session: SessionId,
        #[source]
        source: FactsError<E>,
    },
    #[error("archived session {session} is listed but absent")]
    Missing { session: SessionId },
}
