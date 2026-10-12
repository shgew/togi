use serde::Serialize;
use togi_journal::{Payload, SessionWarning, StateRebuilt};
use togi_machine::object_only;
use togi_tuner::Projection;

use crate::appender::{Appender, Folds};
use crate::error::{AppendFailure, SessionError};
use crate::session::Session;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PublicationPolicy {
    #[default]
    Live,
    OnFinish,
}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct StateDocument {
        pub facts: togi_journal::State,
        pub tuner: Projection,
    }
}

impl StateDocument {
    pub(crate) fn of(folds: &Folds) -> Self {
        Self {
            facts: folds.facts.clone(),
            tuner: folds.tuner.project(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct StateDocumentRef<'a> {
    facts: &'a togi_journal::State,
    tuner: Projection,
}

impl<'a> StateDocumentRef<'a> {
    pub(crate) fn of(folds: &'a Folds) -> Self {
        Self {
            facts: &folds.facts,
            tuner: folds.tuner.project(),
        }
    }
}

impl Appender {
    pub(crate) fn publish_state(&mut self) -> Result<bool, AppendFailure> {
        match self.journal.write_state(&StateDocumentRef::of(&self.folds)) {
            Ok(()) => Ok(true),
            Err(error) => {
                self.publishing = true;
                let durable: Vec<_> = self
                    .journal
                    .events()
                    .last()
                    .map(|event| event.seq)
                    .into_iter()
                    .collect();
                let result = self.append(
                    Payload::SessionWarning(SessionWarning {
                        operation: "write state projection".into(),
                        trial: None,
                        error: error.to_string(),
                    }),
                    &durable,
                );
                self.publishing = false;
                result?;
                Ok(false)
            }
        }
    }
}

impl Session {
    pub(crate) fn rebuild_state(&mut self) -> Result<(), SessionError> {
        if self.appender.adopted {
            return Ok(());
        }
        if self.appender.journal().events().is_empty() {
            return Ok(());
        }
        let fields = match self.appender.journal().read_state::<StateDocument>() {
            Ok(Some(saved)) => {
                let mut fields = Vec::new();
                if saved.facts != self.appender.folds.facts {
                    fields.push("facts".into());
                }
                if saved.tuner != self.appender.tuner().project() {
                    fields.push("tuner".into());
                }
                fields
            }
            Ok(None) | Err(_) => vec!["state.json".into()],
        };
        if !fields.is_empty() && self.appender.publish_state()? {
            self.appender
                .append(Payload::StateRebuilt(StateRebuilt { fields }), &[])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
