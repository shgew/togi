use serde::Serialize;
use togi_journal::{Payload, SessionWarning, StateRebuilt};
use togi_machine::object_only;
use togi_tuner::policy::Projection;

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
        pub recovery: crate::recovery::RecoveryState,
        pub incremental: togi_tuner::policy::State,
        pub defects: togi_defect::DefectState,
        pub presentation: togi_watch::model::Presentation,
    }
}

impl StateDocument {
    pub(crate) fn of(folds: &Folds) -> Self {
        Self {
            facts: folds.facts.clone(),
            tuner: folds.tuner.project(),
            recovery: folds.recovery.clone(),
            incremental: folds.tuner.clone(),
            defects: folds.defects.clone(),
            presentation: folds.presentation.clone(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct StateDocumentRef<'a> {
    facts: &'a togi_journal::State,
    tuner: Projection,
    recovery: &'a crate::recovery::RecoveryState,
    incremental: &'a togi_tuner::policy::State,
    defects: &'a togi_defect::DefectState,
    presentation: &'a togi_watch::model::Presentation,
}

impl<'a> StateDocumentRef<'a> {
    pub(crate) fn of(folds: &'a Folds) -> Self {
        Self {
            facts: &folds.facts,
            tuner: folds.tuner.project(),
            recovery: &folds.recovery,
            incremental: &folds.tuner,
            defects: &folds.defects,
            presentation: &folds.presentation,
        }
    }
}

impl Appender<'_> {
    pub(crate) fn publish_state(&mut self) -> Result<bool, AppendFailure> {
        self.folds.materialize(self.journal.events())?;
        let document =
            togi_journal::snapshot::seal(self.journal.prefix(), &StateDocumentRef::of(&self.folds));
        match document.and_then(|document| self.journal.write_state(&document)) {
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
                        error: togi_render::error_chain(&error),
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

impl Session<'_> {
    pub(crate) fn rebuild_state(&mut self) -> Result<(), SessionError> {
        if self.appender.adopted {
            return Ok(());
        }
        if self.appender.journal().events().is_empty() {
            return Ok(());
        }
        let fields = match self.appender.journal().read_state::<serde_json::Value>() {
            Ok(Some(saved)) => match togi_journal::snapshot::unseal::<StateDocument>(saved) {
                Ok((prefix, saved)) => {
                    let mut fields = differing_facts(&saved.facts, &self.appender.folds.facts);
                    if saved.tuner != self.appender.tuner().project() {
                        fields.push("tuner".into());
                    }
                    if saved.facts.last_seq == self.appender.folds.facts.last_seq {
                        if saved.incremental != self.appender.folds.tuner {
                            fields.push("incremental".into());
                        }
                        if saved.recovery != self.appender.folds.recovery {
                            fields.push("recovery".into());
                        }
                        if saved.defects != self.appender.folds.defects {
                            fields.push("defects".into());
                        }
                        if saved.presentation != self.appender.folds.presentation {
                            fields.push("presentation".into());
                        }
                    }
                    if prefix != self.appender.journal().prefix()
                        && !fields.iter().any(|field| field == "last_seq")
                    {
                        fields.push("snapshot".into());
                    }
                    fields
                }
                Err(togi_journal::snapshot::Rejected::Build) => vec!["build".into()],
                Err(_) => vec!["state.json".into()],
            },
            Ok(None) | Err(_) => vec!["state.json".into()],
        };
        if !fields.is_empty() && self.appender.publish_state()? {
            self.appender
                .append(Payload::StateRebuilt(StateRebuilt { fields }), &[])?;
        }
        Ok(())
    }
}

/// The top-level keys of `facts` on which `saved` and `fresh` disagree, in the order `state.json` writes them.
fn differing_facts(saved: &togi_journal::State, fresh: &togi_journal::State) -> Vec<String> {
    let togi_journal::State {
        schema,
        session,
        cores,
        last_seq,
        open,
        dead_end,
        backends,
    } = saved;
    [
        ("schema", *schema == fresh.schema),
        ("session", *session == fresh.session),
        ("cores", *cores == fresh.cores),
        ("last_seq", *last_seq == fresh.last_seq),
        ("open", *open == fresh.open),
        ("dead_end", *dead_end == fresh.dead_end),
        ("backends", *backends == fresh.backends),
    ]
    .into_iter()
    .filter(|(_, same)| !same)
    .map(|(key, _)| key.into())
    .collect()
}

#[cfg(test)]
mod tests;
