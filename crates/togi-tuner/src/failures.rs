use std::collections::{BTreeMap, HashMap, VecDeque};
use std::num::NonZeroU32;

use togi_journal::{Attribution, Event, Failure, Seq};
use togi_machine::{Condition, CoreId, CoreSet, TargetOffset};

use crate::state::Run;
use crate::{ClassKey, DecisionAction, FoldError, Load, Profile};

/// A failure as the known-failure rule needs it: what a skipped trial copies from it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Witness {
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
    pub(crate) failure: Failure,
    pub(crate) class: ClassKey,
    pub(crate) profile: Profile,
    pub(crate) carried: bool,
    pub(crate) load: Load,
    pub(crate) ledger: Seq,
    pub(crate) cycle: Option<NonZeroU32>,
    pub(crate) round: Option<NonZeroU32>,
}

impl Pending {
    pub(crate) fn offset(&self) -> Option<TargetOffset> {
        self.failure.offset
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
    witnesses: BTreeMap<Seq, Witness>,
    newest: HashMap<(ClassKey, Profile, bool), Seq>,
}

impl Failures {
    /// Keeps `witness` for the ledger fact `seq`, replacing the one it supersedes in its cell.
    pub(crate) fn witness(&mut self, seq: Seq, carried: bool, witness: Witness) {
        let key = (witness.class, witness.profile, carried);
        if let Some(older) = self.newest.insert(key, seq) {
            self.witnesses.remove(&older);
        }
        self.witnesses.insert(seq, witness);
    }

    pub(crate) fn witnessed(&self, seq: Seq) -> Option<&Witness> {
        self.witnesses.get(&seq)
    }

    pub(crate) fn pend(&mut self, seq: Seq, pending: Pending) {
        self.pending.insert(seq, pending);
    }

    pub(crate) fn queue_back(&mut self, seq: Seq) {
        self.queue.push_back(seq);
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

    pub(crate) fn unresolved(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn witnesses(&self) -> usize {
        self.witnesses.len()
    }

    /// Drops what stands for facts a reset of `core` discards.
    pub(crate) fn forget_core(&mut self, core: CoreId) {
        let drops = |class: ClassKey, failure: &Failure| {
            class.loaded().contains(core) || (class.multi_r7() && failure.core == Some(core))
        };
        self.pending
            .retain(|_, record| !drops(record.class, &record.failure));
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
        let awaiting = self.flight.awaiting.as_ref()?;
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
                    .awaiting
                    .as_ref()
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
        let failed_trial = awaiting
            .and_then(|awaiting| awaiting.r7.clone())
            .map(|mut trial| {
                trial.backoffs = self.r7.strikes.snapshot(class.load());
                trial
            });
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
            self.flight.awaiting = None;
        }
        if known_seq.is_some() {
            self.retry = None;
        } else {
            if let Some(core) = failure.core.filter(|_| class.multi_r7()) {
                self.evidence
                    .name_failure(class, &profile, ledger_seq, core)?;
            }
            self.failures.witness(
                ledger_seq,
                false,
                Witness {
                    failure: failure.clone(),
                    class,
                    profile,
                },
            );
        }
        let cited = known_seq.unwrap_or(event.seq);
        if zero_rerun.is_some() && !class.multi_r7() {
            return Ok(());
        }
        let pending = Pending {
            failure: failure.clone(),
            class,
            profile,
            carried: false,
            load,
            ledger: ledger_seq,
            cycle: self.checking.last_cycle(),
            round,
        };
        self.failures.pend(cited, pending.clone());
        if class.multi_r7() {
            if known_seq.is_none() {
                self.r7_open_failure(cited, pending, failed_trial);
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
