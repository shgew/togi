//! Real answers the simulator replays instead of drawing: decisive trial outcomes recorded under a BIOS
//! context. Reading them from an extract is the trial-facts tool's job; this is the validated index.

use rand::{Rng, RngExt};
use std::collections::HashMap;
use std::time::Duration;
use togi_machine::{BiosContext, Regime, Signal, TargetOffset};

use crate::coreset::CoreSet;
use crate::hazard::Load;
use crate::vocab::SignalKind;

#[derive(Clone, Debug, PartialEq)]
pub enum ReplayOutcome {
    Pass,
    /// A failure after `exposure` of the intended duration, which can be zero.
    Failure {
        signal: Signal,
        exposure: Duration,
    },
}

/// One recorded trial: what ran, under which BIOS, with which offsets applied, and what came of it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayFact {
    pub context: BiosContext,
    pub regime: Regime,
    pub workload: String,
    pub cores: CoreSet,
    /// The intended duration.
    pub duration: Duration,
    /// The applied offset of every core, loaded or not.
    pub profile: Vec<TargetOffset>,
    pub outcome: ReplayOutcome,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("replay fact {index}: {problem}")]
pub struct InvalidFact {
    pub index: usize,
    pub problem: FactProblem,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum FactProblem {
    #[error("the BIOS context is empty")]
    NoContext,
    #[error("no loaded cores")]
    NoCores,
    #[error("loaded core {core} is outside the {cores} offsets of the profile")]
    CoreOutsideProfile { core: u8, cores: usize },
    #[error("the intended duration is zero")]
    NoDuration,
    #[error("the exposure exceeds the intended duration")]
    ExposureTooLong,
    #[error("failure signal {0} is not a failure signal")]
    UnsupportedSignal(Signal),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Key {
    regime: Regime,
    workload: String,
    duration: Duration,
    cores: CoreSet,
    profile: Vec<TargetOffset>,
}

/// Validated facts indexed by BIOS context, regime, workload ID, intended duration, loaded cores and
/// the full applied profile. Condition and phase are not part of the key.
#[derive(Clone, Debug, Default)]
pub struct Replay {
    contexts: Vec<(BiosContext, HashMap<Key, Vec<ReplayFact>>)>,
    facts: usize,
}

impl Replay {
    /// Validates and indexes `facts`; facts of one key keep their input order.
    pub fn new(facts: impl IntoIterator<Item = ReplayFact>) -> Result<Self, InvalidFact> {
        let mut replay = Self::default();
        for (index, fact) in facts.into_iter().enumerate() {
            validate(&fact).map_err(|problem| InvalidFact { index, problem })?;
            let position = replay
                .contexts
                .iter()
                .position(|(context, _)| *context == fact.context)
                .unwrap_or_else(|| {
                    replay.contexts.push((fact.context.clone(), HashMap::new()));
                    replay.contexts.len() - 1
                });
            let buckets = &mut replay.contexts[position].1;
            let key = Key {
                regime: fact.regime.clone(),
                workload: fact.workload.clone(),
                duration: fact.duration,
                cores: fact.cores,
                profile: fact.profile.clone(),
            };
            buckets.entry(key).or_default().push(fact);
            replay.facts += 1;
        }
        Ok(replay)
    }

    pub const fn len(&self) -> usize {
        self.facts
    }

    pub const fn is_empty(&self) -> bool {
        self.facts == 0
    }

    fn bucket(
        &self,
        context: &BiosContext,
        load: &Load<'_>,
        profile: &[TargetOffset],
    ) -> Option<&[ReplayFact]> {
        let (_, buckets) = self.contexts.iter().find(|(known, _)| known == context)?;
        let key = Key {
            regime: load.regime.clone(),
            workload: load.workload.to_owned(),
            duration: load.duration,
            cores: load.cores,
            profile: profile.to_vec(),
        };
        buckets.get(&key).map(Vec::as_slice)
    }

    /// Whether a recorded answer exists for this trial under `context`.
    pub fn has_answer(
        &self,
        context: &BiosContext,
        load: &Load<'_>,
        profile: &[TargetOffset],
    ) -> bool {
        self.bucket(context, load, profile).is_some()
    }

    /// A recorded answer chosen uniformly among those matching the trial under `context`.
    pub fn answer(
        &self,
        context: &BiosContext,
        load: &Load<'_>,
        profile: &[TargetOffset],
        rng: &mut impl Rng,
    ) -> Option<&ReplayFact> {
        let bucket = self.bucket(context, load, profile)?;
        bucket.get(rng.random_range(0..bucket.len()))
    }
}

fn validate(fact: &ReplayFact) -> Result<(), FactProblem> {
    if fact.context == BiosContext::default() {
        return Err(FactProblem::NoContext);
    }
    let Some(highest) = fact.cores.highest() else {
        return Err(FactProblem::NoCores);
    };
    if highest.index() >= fact.profile.len() {
        return Err(FactProblem::CoreOutsideProfile {
            core: highest.get(),
            cores: fact.profile.len(),
        });
    }
    if fact.duration.is_zero() {
        return Err(FactProblem::NoDuration);
    }
    match &fact.outcome {
        ReplayOutcome::Pass => Ok(()),
        ReplayOutcome::Failure { signal, exposure } => {
            if SignalKind::of(signal).is_none() && *signal != Signal::UncorrectedMce {
                Err(FactProblem::UnsupportedSignal(signal.clone()))
            } else if *exposure > fact.duration {
                Err(FactProblem::ExposureTooLong)
            } else {
                Ok(())
            }
        }
    }
}
