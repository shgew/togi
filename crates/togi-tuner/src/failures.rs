use std::collections::{BTreeMap, HashMap, VecDeque};
use std::num::NonZeroU32;
use std::sync::Arc;

use togi_journal::{Attribution, Event, Failure, Seq};
use togi_machine::{Condition, CoreId, CoreSet, TargetOffset};

use crate::state::Run;
use crate::{ClassKey, DecisionAction, FoldError, Load, Profile};

/// A failure, its class and the profile it failed under: what the known-failure rule copies to
/// a skipped trial and what a decision answering it reads. A failure that is both witnessed and
/// pending shares one.
#[derive(Debug, PartialEq)]
pub(crate) struct FailureFact {
    pub(crate) failure: Failure,
    pub(crate) class: ClassKey,
    pub(crate) profile: Profile,
}

/// The shape a failure's class gets when no trial intent records one: a single loaded core is a
/// single-core trial.
pub(crate) fn load_of(class: ClassKey) -> Load {
    match class.loaded().first() {
        Some(core) if class.loaded().len() == 1 => Load::Single(core),
        _ => Load::Many(class.loaded()),
    }
}

/// A failure no decision has answered yet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pending {
    pub(crate) fact: Arc<FailureFact>,
    pub(crate) carried: bool,
    pub(crate) load: Load,
    pub(crate) ledger: Seq,
    pub(crate) cycle: Option<NonZeroU32>,
    pub(crate) round: Option<NonZeroU32>,
}

impl Pending {
    pub(crate) fn offset(&self) -> Option<TargetOffset> {
        self.fact.failure.offset
    }
}

/// The unresolved failures, the unattributed ones queued for a hunt, and one witness for the
/// newest failure of each class, profile and origin, which a later trial can be known to repeat.
/// A decision that answers a failure retires its record; a witness stays until the evidence it
/// stands for is discarded.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Failures {
    pending: BTreeMap<Seq, Pending>,
    queue: VecDeque<Seq>,
    witnesses: BTreeMap<Seq, Arc<FailureFact>>,
    newest: HashMap<(ClassKey, Profile, bool), Seq>,
}

impl Failures {
    /// Keeps `fact` as the witness for the ledger fact `seq`, replacing the one it supersedes in
    /// its cell.
    pub(crate) fn witness(&mut self, seq: Seq, carried: bool, fact: Arc<FailureFact>) {
        let key = (fact.class, fact.profile, carried);
        if let Some(older) = self.newest.insert(key, seq) {
            self.witnesses.remove(&older);
        }
        self.witnesses.insert(seq, fact);
    }

    pub(crate) fn witnessed(&self, seq: Seq) -> Option<&FailureFact> {
        self.witnesses.get(&seq).map(Arc::as_ref)
    }

    pub(crate) fn pend(&mut self, seq: Seq, pending: Pending) {
        self.pending.insert(seq, pending);
    }

    pub(crate) fn queue_back(&mut self, seq: Seq) {
        self.queue.push_back(seq);
    }

    pub(crate) fn dequeue_head(&mut self, seq: Seq) {
        if self.queue.front() == Some(&seq) {
            self.queue.pop_front();
        }
    }

    pub(crate) fn requeue(&mut self, seq: Seq) {
        if self.pending.contains_key(&seq) && !self.queue.contains(&seq) {
            self.queue.push_front(seq);
        }
    }

    pub(crate) fn pending(&self, seq: Seq) -> Option<&Pending> {
        self.pending.get(&seq)
    }

    pub(crate) fn retire(&mut self, seq: Seq) {
        if self.pending.remove(&seq).is_some() {
            self.queue.retain(|&queued| queued != seq);
        }
    }

    pub(crate) fn retire_all(&mut self, seqs: &[Seq]) {
        for &seq in seqs {
            self.retire(seq);
        }
    }

    pub(crate) fn queue_is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub(crate) fn queue(&self) -> impl Iterator<Item = Seq> + '_ {
        self.queue.iter().copied()
    }

    #[cfg(test)]
    pub(crate) fn unresolved(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub(crate) fn witnesses(&self) -> usize {
        self.witnesses.len()
    }

    /// Drops what stands for facts a reset of `core` discards.
    pub(crate) fn forget_core(&mut self, core: CoreId) {
        let drops = |class: ClassKey, failure: &Failure| {
            class.loaded().contains(core) || (class.multi_r7() && failure.core == Some(core))
        };
        self.pending
            .retain(|_, record| !drops(record.fact.class, &record.fact.failure));
        let pending = &self.pending;
        self.queue.retain(|seq| pending.contains_key(seq));
        self.witnesses
            .retain(|_, witness| !drops(witness.class, &witness.failure));
        let witnesses = &self.witnesses;
        self.newest.retain(|_, seq| witnesses.contains_key(seq));
    }
}

/// The core a together or parked failure names: the one the trial end names, else the one
/// core-local MCE the end cites, else outside multi-core R7 the only core off zero. Several
/// cited cores, or none to go by, name no core.
pub(crate) fn together_attribution(
    ended_on: Option<CoreId>,
    cited: CoreSet,
    profile: &Profile,
    multi_r7: bool,
) -> (Attribution, Option<CoreId>, Option<TargetOffset>) {
    let named: Vec<CoreId> = match ended_on {
        Some(core) => vec![core],
        None => cited.iter().collect(),
    };
    let named = if named.is_empty() && !multi_r7 {
        profile.sole_nonzero().into_iter().collect()
    } else {
        named
    };
    match named[..] {
        [core] if profile.get(core).is_some() => {
            (Attribution::Attributed, Some(core), profile.get(core))
        }
        _ => (Attribution::Unattributed, None, None),
    }
}

impl Run {
    /// The attribution of the failed trial awaiting one: alone failures belong to the target;
    /// together and parked ones to the one core a backend signal or a core-local MCE the end
    /// cites names, else the only core off zero, else to no core.
    pub(crate) fn attribution(&self) -> Option<DecisionAction> {
        let awaiting = self.flight.awaiting()?;
        let intent = &awaiting.intent;
        let (attribution, core, offset) = if intent.condition == Condition::Alone {
            (Attribution::Attributed, intent.core, intent.offset)
        } else {
            together_attribution(
                awaiting.core,
                awaiting.mces,
                &awaiting.profile,
                awaiting.class.multi_r7(),
            )
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
                    .awaiting()
                    .filter(|awaiting| &awaiting.intent.trial == trial)
                    .ok_or_else(|| FoldError::NotAwaiting(trial.clone()))?,
            ),
            (Some(_) | None, Some(_)) | (None, None) => None,
        };
        let known = match failure.known_failure {
            Some(seq) => Some(
                self.failures
                    .witnessed(seq)
                    .ok_or(FoldError::UnknownFailure(seq))?,
            ),
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
        let ledger_seq = awaiting.map_or(event.seq, |awaiting| awaiting.end_seq);
        let known_seq = failure.known_failure;
        let zero_rerun = awaiting.and_then(|awaiting| awaiting.zero);
        let failed_trial = match awaiting.and_then(|awaiting| awaiting.r7.clone()) {
            Some(mut trial) => {
                trial.backoffs = self.r7.strikes.snapshot(class.load());
                trial.located_loaded = self.r7_loaded_verdict(class.load(), &profile);
                trial.cover = self.evidence.loaded_cover(class.load(), &profile)?;
                Some(trial)
            }
            None => None,
        };
        let load = match awaiting {
            Some(awaiting) => match awaiting.intent.core {
                Some(core) if awaiting.intent.cores.is_empty() => Load::Single(core),
                _ => Load::Many(class.loaded()),
            },
            None => crate::failures::load_of(class),
        };
        let round = failure
            .round
            .or_else(|| awaiting.and_then(|awaiting| awaiting.intent.round));

        if awaiting.is_some() {
            self.flight.current = crate::flight::InFlight::Idle;
        }
        let fact = Arc::new(FailureFact {
            failure: failure.clone(),
            class,
            profile,
        });
        if known_seq.is_some() {
            self.retry = None;
        } else {
            if let Some(core) = failure.core.filter(|_| class.multi_r7()) {
                self.evidence
                    .name_failure(class, &profile, ledger_seq, core)?;
            }
            self.failures.witness(ledger_seq, false, Arc::clone(&fact));
        }
        let cited = known_seq.unwrap_or(event.seq);
        if known_seq.is_none() {
            self.hunt_note_parked_failure(event.seq, class, failure, false);
        }
        if zero_rerun.is_some() && !class.multi_r7() {
            return Ok(());
        }
        let pending = Pending {
            fact,
            carried: false,
            load,
            ledger: ledger_seq,
            cycle: self.checking.last_cycle(),
            round,
        };
        self.failures.pend(cited, pending.clone());
        if class.multi_r7() {
            if known_seq.is_none() {
                if zero_rerun.is_none() {
                    self.hunt_keep_rejecting(ledger_seq, cited, &pending, failed_trial.as_ref());
                }
                self.r7_open_failure(cited, pending, failed_trial);
                self.hunt_located_culprit(cited, class, failure);
            }
            self.phase2_failure(cited);
            return Ok(());
        }
        if failure.attribution == Attribution::Attributed {
            if let Some(core) = failure.core {
                let replaced = self.core(core).pending;
                if let Some(old) = replaced.filter(|&old| old != cited) {
                    self.failures.retire(old);
                    self.prune_blames();
                }
                self.update_core(core, |state| state.pending = Some(cited));
            }
        }
        let together = failure.condition == Some(Condition::Together);
        let parked = failure.condition == Some(Condition::Parked);
        let unattributed = failure.attribution == Attribution::Unattributed;
        if unattributed && (together || (parked && failure.trial.is_none())) {
            self.failures.queue_back(cited);
        }
        if unattributed && known_seq.is_none() && failure.trial.is_none() && (together || parked) {
            self.live_idle_failure(event.seq, &profile)?;
        }
        self.phase2_failure(cited);
        Ok(())
    }
}
