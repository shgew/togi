//! Phase-2 rounds: the round a deepening starts, the checks that verify it, and the blame a
//! failure during a round or the confirmation cycle gets. The checks and the blame are worked
//! out when the event that changes them folds; the decision functions only read.

mod actions;
mod blame;
mod checks;
mod context;

use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{DeepeningRound, Event, RoundEvent, Seq};
use togi_machine::{Condition, CoreId, CoreSet, TargetOffset};

use crate::state::Run;
use crate::{ClassKey, Depth, FoldError, Profile, Secs};

pub(crate) use blame::Blame;

/// The shallower of two offsets.
pub(crate) fn shallowest(a: TargetOffset, b: TargetOffset) -> TargetOffset {
    if b.shallower_than(a) { b } else { a }
}

/// What a round was: where it began, the cores it moved, the offsets they stood at and the
/// profile it proposed. A core is deepened when its proposed offset is deeper than its baseline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Shape {
    id: NonZeroU32,
    start: Seq,
    cores: CoreSet,
    baseline: Profile,
    profile: Profile,
}

impl Shape {
    pub(crate) const fn id(&self) -> NonZeroU32 {
        self.id
    }

    pub(crate) const fn start(&self) -> Seq {
        self.start
    }

    pub(crate) const fn cores(&self) -> CoreSet {
        self.cores
    }

    pub(crate) const fn baseline(&self) -> &Profile {
        &self.baseline
    }

    pub(crate) const fn profile(&self) -> &Profile {
        &self.profile
    }

    pub(crate) fn deepened(&self, core: CoreId) -> bool {
        self.cores.contains(core)
            && matches!(
                (self.profile.get(core), self.baseline.get(core)),
                (Some(proposed), Some(base)) if proposed.deeper_than(base)
            )
    }
}

/// One check the round needs: `needed` passes of `class` on `profile` since `since`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Check {
    pub(crate) class: ClassKey,
    pub(crate) condition: Condition,
    pub(crate) profile: Profile,
    pub(crate) since: Seq,
}

/// The round in progress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Round {
    shape: Shape,
    target: Profile,
    trials: NonZeroU16,
    trial_s: Secs,
    checks: Vec<Check>,
    sources: Vec<Seq>,
}

impl Round {
    pub(crate) const fn shape(&self) -> Shape {
        self.shape
    }

    pub(crate) const fn id(&self) -> NonZeroU32 {
        self.shape.id
    }

    pub(crate) const fn start(&self) -> Seq {
        self.shape.start
    }

    pub(crate) const fn profile(&self) -> &Profile {
        &self.shape.profile
    }

    pub(crate) const fn cores(&self) -> CoreSet {
        self.shape.cores
    }

    pub(crate) const fn target(&self) -> &Profile {
        &self.target
    }

    pub(crate) const fn trials(&self) -> NonZeroU16 {
        self.trials
    }

    pub(crate) const fn trial_s(&self) -> Secs {
        self.trial_s
    }

    pub(crate) fn checks(&self) -> &[Check] {
        &self.checks
    }

    /// The measurements that ordered the R7 checks.
    pub(crate) fn sources(&self) -> &[Seq] {
        &self.sources
    }
}

/// The round in progress, the last one that ended and the round ids used so far.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Rounds {
    active: Option<Round>,
    last: Option<Shape>,
    used: u32,
}

impl Rounds {
    pub(crate) fn active(&self) -> Option<&Round> {
        self.active.as_ref()
    }

    pub(crate) const fn last(&self) -> Option<&Shape> {
        self.last.as_ref()
    }

    pub(crate) fn next_id(&self) -> NonZeroU32 {
        NonZeroU32::new(self.used.saturating_add(1)).unwrap_or(NonZeroU32::MAX)
    }

    pub(crate) fn reset(&mut self) {
        self.active = None;
        self.last = None;
    }
}

impl Run {
    pub(crate) fn deepening_round(
        &mut self,
        event: &Event,
        round: &DeepeningRound,
    ) -> Result<(), FoldError> {
        match round.event {
            RoundEvent::Start => self.round_start(event.seq, round)?,
            RoundEvent::End => self.round_end(round),
        }
        self.reruns.consume_citations();
        self.revalidate_retry();
        Ok(())
    }

    fn round_start(&mut self, seq: Seq, round: &DeepeningRound) -> Result<(), FoldError> {
        let profile = Profile::new(&self.topo, round.profile)?;
        let target = Profile::new(&self.topo, round.target)?;
        Profile::new(&self.topo, round.base)?;
        self.require_cores(round.cores)?;
        let offsets = self.offsets();
        let baseline = self.phases.passed().map_or(offsets, |passed| {
            Profile::over(self.topo.all(), |core| {
                shallowest(
                    offsets.get(core).unwrap_or(TargetOffset::ZERO),
                    passed.get(core).unwrap_or(TargetOffset::ZERO),
                )
            })
        });
        let shape = Shape {
            id: round.round,
            start: seq,
            cores: round.cores,
            baseline,
            profile,
        };
        self.phases.round_started(&shape, &offsets);
        let mut started = Round {
            shape,
            target,
            trials: round
                .trials
                .and_then(|trials| u16::try_from(trials).ok())
                .and_then(NonZeroU16::new)
                .unwrap_or(self.config.needed),
            trial_s: round
                .trial_s
                .and_then(NonZeroU32::new)
                .map_or(self.config.short_trial, Secs::new),
            checks: Vec::new(),
            sources: Vec::new(),
        };
        (started.checks, started.sources) = self.round_checks(&started);
        self.rounds.used = self.rounds.used.max(round.round.get());
        self.last_plan_seq = Some(seq);
        self.rounds.active = Some(started);
        Ok(())
    }

    fn round_end(&mut self, round: &DeepeningRound) {
        if let Some(ended) = self
            .rounds
            .active
            .take_if(|active| active.id() == round.round)
        {
            let shape = ended.shape();
            self.phases.round_ended(&shape, round.passed);
            self.rounds.last = Some(shape);
        }
    }

    /// A request measurement folded or was discarded: the R7 checks follow the order it gives.
    pub(crate) fn refresh_round_checks(&mut self) {
        let Some(mut round) = self.rounds.active.take() else {
            return;
        };
        (round.checks, round.sources) = self.round_checks(&round);
        self.rounds.active = Some(round);
        self.revalidate_retry();
    }

    /// A core entered search: phase 2 starts over and no round stands.
    pub(crate) fn rounds_reset(&mut self) {
        self.rounds.reset();
        self.revalidate_retry();
    }
}
