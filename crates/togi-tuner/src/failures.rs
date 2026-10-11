use std::collections::{BTreeMap, VecDeque};

use togi_journal::{Attribution, Event, Failure};
use togi_machine::{Condition, CoreId, Regime};

use crate::flight::Awaiting;
use crate::state::Run;
use togi_journal::Seq;

use crate::{ClassKey, DecisionAction, FoldError, Profile};

/// A failure the tuner knows under the seq of the event that recorded it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FailureRecord {
    pub(crate) seq: Seq,
    pub(crate) failure: Failure,
    pub(crate) class: ClassKey,
    pub(crate) profile: Profile,
    pub(crate) carried: bool,
}

/// The failures later trials can be known to repeat, and the unattributed ones queued for a hunt.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Failures {
    records: BTreeMap<Seq, FailureRecord>,
    aliases: BTreeMap<Seq, Seq>,
    queue: VecDeque<Seq>,
}

impl Failures {
    pub(crate) fn insert(&mut self, record: FailureRecord) {
        self.records.insert(record.seq, record);
    }

    fn alias(&mut self, seq: Seq, record: Seq) {
        self.aliases.insert(seq, record);
    }

    /// The failure known as `seq`: its own event, or the failed `trial.end` it attributed.
    pub(crate) fn get(&self, seq: Seq) -> Option<&FailureRecord> {
        self.records
            .get(self.aliases.get(&seq).unwrap_or(&seq))
    }

    pub(crate) fn queue_is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub(crate) fn queue(&self) -> impl Iterator<Item = Seq> + '_ {
        self.queue.iter().copied()
    }

    pub(crate) fn queued(&self) -> Option<&FailureRecord> {
        self.queue.front().and_then(|seq| self.records.get(seq))
    }

    pub(crate) fn forget_core(&mut self, core: CoreId) {
        let drops = |record: &FailureRecord| {
            record.class.loaded().contains(core)
                || (record.class.multi_r7() && record.failure.core == Some(core))
        };
        self.records.retain(|_, record| !drops(record));
        let records = &self.records;
        self.aliases.retain(|_, record| records.contains_key(record));
        self.queue.retain(|seq| records.contains_key(seq));
    }
}

fn unattributed_attribution(
    run: &Run,
    awaiting: &Awaiting,
) -> (Attribution, Option<CoreId>, Option<togi_machine::TargetOffset>) {
    let intent = &awaiting.intent;
    let named: Vec<CoreId> = match awaiting.core {
        Some(core) => vec![core],
        None => awaiting.mces.iter().collect(),
    };
    let named = if named.is_empty()
        && (intent.regime != Regime::R7 || intent.cores.len() <= 1)
    {
        awaiting.profile.sole_nonzero().into_iter().collect()
    } else {
        named
    };
    match named[..] {
        [core] if run.topo.contains(core) => (
            Attribution::Attributed,
            Some(core),
            Some(awaiting.profile.get(core)),
        ),
        _ => (Attribution::Unattributed, None, None),
    }
}

impl Run {
    /// The attribution of the failed trial awaiting one: alone failures belong to the target;
    /// together and parked ones to the one core a backend signal or core-local MCE names, else
    /// the only core off zero, else to no core.
    pub(crate) fn attribution(&self) -> Option<DecisionAction> {
        let awaiting = self.flight.awaiting.as_ref()?;
        let intent = &awaiting.intent;
        let (attribution, core, offset) = if intent.condition == Condition::Alone {
            (Attribution::Attributed, intent.core, intent.offset)
        } else {
            unattributed_attribution(self, awaiting)
        };
        let mut cause = vec![awaiting.end_seq];
        cause.extend(awaiting.cause.iter().copied());
        Some(DecisionAction::new(
            Failure {
                signal: awaiting.signal,
                attribution,
                core,
                offset,
                trial: Some(intent.trial.clone()),
                regime: Some(intent.regime),
                condition: Some(intent.condition),
                profile: Some(awaiting.profile.offsets()),
                known_failure: None,
                reason: None,
                round: None,
            },
            cause,
        ))
    }

    pub(crate) fn failure(&mut self, event: &Event, failure: &Failure) -> Result<(), FoldError> {
        if let Some(core) = failure.core {
            self.require_core(core)?;
        }
        let awaiting = match (&failure.trial, failure.known_failure) {
            (Some(trial), None) => Some(
                self.flight
                    .awaiting
                    .as_ref()
                    .filter(|awaiting| &awaiting.intent.trial == trial)
                    .ok_or_else(|| FoldError::NotAwaiting(trial.clone()))?,
            ),
            (Some(_) | None, Some(_)) | (None, None) => None,
        };
        let known = match failure.known_failure {
            Some(seq) => Some(self.failures.get(seq).ok_or(FoldError::UnknownFailure(seq))?),
            None => None,
        };
        let loaded = matches!(
            failure.condition,
            Some(Condition::Together | Condition::Parked)
        );
        let (class, mut profile) = match (known, awaiting) {
            (Some(known), _) => (known.class, known.profile),
            (None, Some(awaiting)) => (awaiting.class, awaiting.profile),
            (None, None) if failure.trial.is_none() && loaded => {
                (self.idle_class(), self.offsets())
            }
            (None, None) => return Err(FoldError::NoFailureClass),
        };
        if let Some(recorded) = failure.profile {
            profile = Profile::new(&self.topo, recorded)?;
        } else if awaiting.is_none() && known.is_none() {
            profile = self.checking.profile().unwrap_or(profile);
        }
        let end_seq = awaiting.map(|awaiting| awaiting.end_seq);

        if end_seq.is_some() {
            self.flight.awaiting = None;
        }
        if let Some(end) = end_seq {
            self.failures.alias(end, event.seq);
        }
        if failure.known_failure.is_some() {
            self.retry = None;
        }
        self.failures.insert(FailureRecord {
            seq: event.seq,
            failure: failure.clone(),
            class,
            profile,
            carried: false,
        });
        if class.multi_r7() {
            return Ok(());
        }
        if failure.attribution == Attribution::Attributed {
            if let Some(core) = failure.core {
                self.update_core(core, |state| state.pending = Some(event.seq));
            }
        }
        let together = failure.condition == Some(Condition::Together);
        let parked = failure.condition == Some(Condition::Parked);
        let unattributed = failure.attribution == Attribution::Unattributed;
        if unattributed && (together || (parked && failure.trial.is_none())) {
            self.failures.queue.push_back(event.seq);
        }
        if unattributed
            && failure.known_failure.is_none()
            && failure.trial.is_none()
            && (together || parked)
        {
            self.live_idle_failure(event.seq, &profile);
        }
        Ok(())
    }
}
