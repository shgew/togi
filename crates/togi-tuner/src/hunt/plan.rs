use togi_journal::{HuntOutcome, Seq};
use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use crate::r7::Applied;
use crate::{Constraints, Depth, Profile, Ranking};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Stage {
    Part,
    Complement,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Duration {
    Short,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailedDuration {
    NoLonger,
    Longer,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum GroupStage {
    Locate,
    Part,
    Complement,
    Full,
    Repeated,
}

/// How a settled group came out; a skipped group supplies no pass evidence but advances the plan
/// like a passed one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GroupOutcome {
    Passed,
    Failed,
    Skipped,
}

/// Where a plan finished. A combination still needs member probes unless the profile already
/// breaks it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum End {
    Culprit(CoreId),
    Combination(CoreSet),
    Fallback(CoreSet),
    Loaded,
}

impl End {
    pub(crate) fn outcome(self) -> HuntOutcome {
        match self {
            Self::Culprit(_) => HuntOutcome::Culprit,
            Self::Combination(_) => HuntOutcome::Combination,
            Self::Fallback(_) => HuntOutcome::Fallback,
            Self::Loaded => HuntOutcome::Loaded,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum PlanError {
    #[error("a hunt needs at least one candidate")]
    NoCandidates,
}

/// A located hunt keeps the loaded cores at their failing offsets and narrows only the unloaded
/// candidates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Located {
    pub(crate) loaded_at_zero: bool,
}

/// Everything the plan needs from the hunt's start. `recent` is the hunt-start snapshot of the
/// recent constraint cores. `longer_failure` is whether the failed duration exceeds the short
/// duration; `duration_prior` starts the hunt at the failed duration. `repeated_core` is the
/// caller's verdict on the repeated-core probe.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Start {
    pub(crate) candidates: CoreSet,
    pub(crate) loaded: CoreSet,
    pub(crate) recent: CoreSet,
    pub(crate) longer_failure: bool,
    pub(crate) duration_prior: bool,
    pub(crate) located: Option<Located>,
    pub(crate) repeated_core: Option<CoreId>,
}

/// The next group to run: `cores` take their failing offsets, every other core its parked offset.
/// The other fields are the plan state recorded with the group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Group {
    pub(crate) stage: GroupStage,
    pub(crate) cores: CoreSet,
    pub(crate) set: CoreSet,
    pub(crate) granularity: usize,
    pub(crate) index: usize,
    pub(crate) duration: Duration,
    pub(crate) escalated: bool,
    pub(crate) full_checked: bool,
    pub(crate) any_failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Locate,
    Repeated(CoreId),
    Narrow,
    Full,
    Done(End),
}

/// The delta-debugging plan of one hunt as a value: `next_group` says what to run, `advance`
/// takes the settled outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Plan {
    candidates: CoreSet,
    loaded: CoreSet,
    recent: CoreSet,
    failed: FailedDuration,
    located: Option<Located>,
    repeated_core: Option<CoreId>,
    set: CoreSet,
    granularity: usize,
    stage: Stage,
    index: usize,
    duration: Duration,
    escalated: bool,
    full_checked: bool,
    any_failed: bool,
    step: Step,
}

impl Plan {
    pub(crate) fn new(start: Start) -> Result<Self, PlanError> {
        if start.candidates.is_empty() {
            return Err(PlanError::NoCandidates);
        }
        let repeated_core = start
            .repeated_core
            .filter(|&core| start.candidates.len() > 2 && start.candidates.contains(core));
        let mut plan = Self {
            candidates: start.candidates,
            loaded: start.loaded,
            recent: start.recent,
            failed: if start.longer_failure {
                FailedDuration::Longer
            } else {
                FailedDuration::NoLonger
            },
            located: start.located,
            repeated_core,
            set: start.candidates,
            granularity: 2,
            stage: Stage::Part,
            index: 0,
            duration: if start.duration_prior {
                Duration::Failed
            } else {
                Duration::Short
            },
            escalated: start.duration_prior,
            full_checked: false,
            any_failed: false,
            step: Step::Narrow,
        };
        plan.step = if plan.located.is_some() {
            Step::Locate
        } else {
            plan.opening()
        };
        Ok(plan)
    }

    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) fn granularity(&self) -> usize {
        self.granularity
    }

    pub(crate) fn escalated(&self) -> bool {
        self.escalated
    }

    pub(crate) fn full_checked(&self) -> bool {
        self.full_checked
    }

    /// The remaining candidates; the culprit or combination once the plan has ended.
    pub(crate) fn set(&self) -> CoreSet {
        self.set
    }

    pub(crate) fn end(&self) -> Option<End> {
        match self.step {
            Step::Done(end) => Some(end),
            _ => None,
        }
    }

    /// The parts of the current set at the current granularity, in the order they are tried.
    pub(crate) fn parts(&self) -> Vec<CoreSet> {
        let split = (self.set == self.candidates).then_some(self.loaded);
        partition(self.set, self.granularity, split, self.recent)
    }

    pub(crate) fn next_group(&self) -> Option<Group> {
        let group = |stage, cores| Group {
            stage,
            cores,
            set: self.set,
            granularity: self.granularity,
            index: self.index,
            duration: self.duration,
            escalated: self.escalated,
            full_checked: self.full_checked,
            any_failed: self.any_failed,
        };
        match self.step {
            Step::Done(_) => None,
            Step::Locate => Some(Group {
                set: self.candidates,
                granularity: 2,
                index: 0,
                duration: Duration::Failed,
                escalated: false,
                full_checked: false,
                any_failed: false,
                ..group(GroupStage::Locate, CoreSet::default())
            }),
            Step::Repeated(core) => Some(Group {
                granularity: self.candidates.len(),
                index: 0,
                ..group(GroupStage::Repeated, std::iter::once(core).collect())
            }),
            Step::Full => Some(Group {
                index: 0,
                ..group(GroupStage::Full, self.set)
            }),
            Step::Narrow => {
                let part = *self.parts().get(self.index)?;
                Some(match self.stage {
                    Stage::Part => group(GroupStage::Part, part),
                    Stage::Complement => group(
                        GroupStage::Complement,
                        self.set.iter().filter(|&c| !part.contains(c)).collect(),
                    ),
                })
            }
        }
    }

    /// Takes the outcome of the group `next_group` returned. A finished plan ignores it.
    pub(crate) fn advance(&mut self, outcome: GroupOutcome) {
        match self.step {
            Step::Done(_) => {}
            Step::Locate => {
                self.step = match outcome {
                    GroupOutcome::Passed => self.opening(),
                    GroupOutcome::Failed | GroupOutcome::Skipped => Step::Done(End::Loaded),
                };
            }
            Step::Repeated(core) => match outcome {
                GroupOutcome::Failed => {
                    self.any_failed = true;
                    self.set = std::iter::once(core).collect();
                    self.step = Step::Done(End::Culprit(core));
                }
                GroupOutcome::Passed | GroupOutcome::Skipped => self.step = Step::Narrow,
            },
            Step::Narrow => self.advance_narrowing(outcome),
            Step::Full => self.advance_full(outcome),
        }
    }

    fn advance_narrowing(&mut self, outcome: GroupOutcome) {
        let Some(group) = self.next_group() else {
            return;
        };
        if outcome == GroupOutcome::Failed {
            self.any_failed = true;
            self.set = group.cores;
            self.granularity = match self.stage {
                Stage::Part => 2,
                Stage::Complement => self.granularity.saturating_sub(1).max(2),
            };
            self.stage = Stage::Part;
            self.index = 0;
            self.step = self.opening_narrowed();
            return;
        }
        if self.index + 1 < self.parts().len() {
            self.index += 1;
        } else if self.stage == Stage::Part && self.granularity > 2 {
            self.stage = Stage::Complement;
            self.index = 0;
        } else {
            self.exhausted();
        }
    }

    fn advance_full(&mut self, outcome: GroupOutcome) {
        self.full_checked = true;
        if self.located.is_some() && self.duration == Duration::Failed {
            self.any_failed |= outcome == GroupOutcome::Failed;
            self.step = Step::Done(self.ending());
            return;
        }
        match outcome {
            GroupOutcome::Passed => {
                self.escalated = true;
                self.duration = Duration::Failed;
                self.set = self.candidates;
                self.granularity = 2;
                self.stage = Stage::Part;
                self.index = 0;
                self.full_checked = false;
                self.any_failed = false;
                self.step = self.opening_narrowed();
            }
            GroupOutcome::Failed => {
                self.any_failed = true;
                self.finer();
            }
            GroupOutcome::Skipped => self.finer(),
        }
    }

    fn exhausted(&mut self) {
        if !self.escalated
            && !self.full_checked
            && self.granularity == 2
            && self.set == self.candidates
            && self.failed == FailedDuration::Longer
        {
            self.index = 0;
            self.step = Step::Full;
        } else {
            self.finer();
        }
    }

    fn finer(&mut self) {
        if self.granularity < self.set.len() {
            self.granularity = (self.granularity * 2).min(self.set.len());
            self.stage = Stage::Part;
            self.index = 0;
            self.step = Step::Narrow;
        } else if self.located.is_some() && !self.any_failed {
            self.duration = Duration::Failed;
            self.escalated |= self.failed == FailedDuration::Longer;
            self.index = 0;
            self.step = Step::Full;
        } else {
            self.step = Step::Done(self.ending());
        }
    }

    fn opening(&self) -> Step {
        match (self.sole(), self.repeated_core) {
            (Some(core), _) => Step::Done(End::Culprit(core)),
            (None, Some(core)) => Step::Repeated(core),
            (None, None) => Step::Narrow,
        }
    }

    fn opening_narrowed(&self) -> Step {
        match self.sole() {
            Some(core) => Step::Done(End::Culprit(core)),
            None => Step::Narrow,
        }
    }

    fn sole(&self) -> Option<CoreId> {
        let mut cores = self.set.iter();
        let core = cores.next()?;
        cores.next().is_none().then_some(core)
    }

    fn ending(&self) -> End {
        if let Some(core) = self.sole() {
            End::Culprit(core)
        } else if self.any_failed {
            End::Combination(self.set)
        } else if self.located.is_some_and(|l| !l.loaded_at_zero) {
            End::Loaded
        } else {
            End::Fallback(self.set)
        }
    }
}

/// Splits `set` into `granularity` pieces (clamped to `1..=len`). `split` is the loaded cores when
/// `set` is the hunt's original candidates: at granularity 2, with both loaded and unloaded
/// candidates, the pieces are the loaded then the unloaded ones. Otherwise the pieces are
/// contiguous in core-id order, their sizes differing by at most one with the earlier pieces
/// taking the remainder. Pieces holding a `recent` core come first, the order otherwise kept.
pub(crate) fn partition(
    set: CoreSet,
    granularity: usize,
    split: Option<CoreSet>,
    recent: CoreSet,
) -> Vec<CoreSet> {
    if set.is_empty() {
        return Vec::new();
    }
    let granularity = granularity.clamp(1, set.len());
    let mut parts = split
        .filter(|_| granularity == 2)
        .map(|loaded| {
            let busy: CoreSet = set.iter().filter(|&c| loaded.contains(c)).collect();
            let idle: CoreSet = set.iter().filter(|&c| !loaded.contains(c)).collect();
            (busy, idle)
        })
        .filter(|(busy, idle)| !busy.is_empty() && !idle.is_empty())
        .map_or_else(Vec::new, |(busy, idle)| vec![busy, idle]);
    if parts.is_empty() {
        let (size, extra) = (set.len() / granularity, set.len() % granularity);
        let mut cores = set.iter();
        parts = (0..granularity)
            .map(|piece| {
                let take = size + usize::from(piece < extra);
                cores.by_ref().take(take).collect()
            })
            .collect();
    }
    parts.sort_by_key(|part| !part.iter().any(|core| recent.contains(core)));
    parts
}

/// The profile hunt groups park the other cores at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Parked {
    pub(crate) profile: Profile,
    pub(crate) source: Option<Seq>,
    pub(crate) candidates: CoreSet,
}

/// Parks every core at the shallower of the newest usable passed full-cycle offset and its failing
/// offset. A passed profile nowhere shallower than the failing one cannot supply the parked
/// offsets, so older ones are tried in the order given (newest first), then all-zero. The
/// candidates are the cores deeper at the failure than parked.
pub(crate) fn park<'a>(
    failing: &Profile,
    passed: impl IntoIterator<Item = (Seq, &'a Profile)>,
) -> Parked {
    let raised = passed.into_iter().find_map(|(seq, profile)| {
        let raised = Profile::over(failing.cores(), |core| {
            let offset = profile.at(core);
            let fails_at = failing.at(core);
            if offset.shallower_than(fails_at) {
                offset
            } else {
                fails_at
            }
        });
        (raised != *failing).then_some((seq, raised))
    });
    let (source, profile) = match raised {
        Some((seq, profile)) => (Some(seq), profile),
        None => (None, Profile::over(failing.cores(), |_| TargetOffset::ZERO)),
    };
    let candidates = failing
        .iter()
        .filter(|&(core, offset)| offset.deeper_than(profile.at(core)))
        .map(|(core, _)| core)
        .collect();
    Parked {
        profile,
        source,
        candidates,
    }
}

/// A located hunt's parking: loaded cores keep their failing offsets, every unloaded core parks
/// at zero, and the candidates are the unloaded cores that failed off zero.
pub(crate) fn park_located(failing: &Profile, loaded: CoreSet) -> Parked {
    let profile = Profile::over(failing.cores(), |core| {
        if loaded.contains(core) {
            failing.at(core)
        } else {
            TargetOffset::ZERO
        }
    });
    let candidates = failing
        .iter()
        .filter(|&(core, offset)| !loaded.contains(core) && offset != TargetOffset::ZERO)
        .map(|(core, _)| core)
        .collect();
    Parked {
        profile,
        source: None,
        candidates,
    }
}

/// The parked profile with `cores` at their failing offsets.
pub(crate) fn group_profile(parked: &Profile, failing: &Profile, cores: CoreSet) -> Profile {
    cores.iter().fold(*parked, |profile, core| {
        profile.with(core, failing.at(core))
    })
}

/// A member probe: `member` at `offset` with the other combination members `held` at their
/// current shallowest failing offsets and every other core parked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Probe {
    pub(crate) member: CoreId,
    pub(crate) offset: TargetOffset,
    pub(crate) held: PerCore<TargetOffset>,
    pub(crate) index: usize,
}

impl Probe {
    pub(crate) fn profile(&self, parked: &Profile) -> Profile {
        self.held
            .iter()
            .fold(*parked, |profile, (core, offset)| {
                profile.with(core, offset)
            })
            .with(self.member, self.offset)
    }

    /// Whether every held member sits at its original failing offset, so the probe moved only
    /// its own member.
    pub(crate) fn holds_failing(&self, failing: &Profile) -> bool {
        self.held
            .iter()
            .all(|(core, offset)| offset == failing.at(core))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProbeResult {
    pub(crate) member: CoreId,
    pub(crate) offset: TargetOffset,
    pub(crate) outcome: GroupOutcome,
}

/// The shallowest failing offset found for each member; `stopped` when a skipped probe ended the
/// probing early.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Found {
    pub(crate) members: PerCore<TargetOffset>,
    pub(crate) stopped: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProbeStep {
    Run(Probe),
    Done(Found),
}

/// Member probing of a combination, one member at a time in core-id order. The next step is a
/// function of all settled probe results, so a result that later changes is read again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemberProbes {
    members: CoreSet,
    failing: Profile,
    parked: Profile,
}

impl MemberProbes {
    pub(crate) fn new(members: CoreSet, failing: &Profile, parked: &Profile) -> Self {
        Self {
            members,
            failing: *failing,
            parked: *parked,
        }
    }

    /// `results` are the settled probes in the order they ran. A member's probes go 1, 2, 4, …
    /// counts shallower than its failing offset until one passes or would reach the shallowest
    /// pass, then bisect between its deepest failure and shallowest pass until they are one count
    /// apart.
    pub(crate) fn step(&self, results: &[ProbeResult]) -> ProbeStep {
        let mut found: PerCore<TargetOffset> = self
            .members
            .iter()
            .map(|core| (core, self.failing.at(core)))
            .collect();
        for member in self.members {
            let origin = self.failing.at(member);
            let (mut lo, mut hi) = (origin, self.parked.at(member));
            let (mut failed, mut passed) = (0u32, false);
            for result in results.iter().filter(|r| r.member == member) {
                match result.outcome {
                    GroupOutcome::Failed => {
                        if result.offset.shallower_than(lo) {
                            lo = result.offset;
                        }
                        failed += 1;
                    }
                    GroupOutcome::Passed => {
                        if result.offset.deeper_than(hi) {
                            hi = result.offset;
                        }
                        passed = true;
                    }
                    GroupOutcome::Skipped => {
                        found.set(member, lo);
                        return ProbeStep::Done(Found {
                            members: found,
                            stopped: true,
                        });
                    }
                }
            }
            found.set(member, lo);
            if hi.get() - lo.get() <= 1 {
                continue;
            }
            let gallop = origin
                .get()
                .saturating_add(1i64.checked_shl(failed).unwrap_or(i64::MAX));
            let offset = if !passed && gallop > lo.get() && gallop < hi.get() {
                gallop
            } else {
                lo.get() + (hi.get() - lo.get()) / 2
            };
            let held = found.iter().filter(|&(core, _)| core != member).collect();
            return ProbeStep::Run(Probe {
                member,
                offset: TargetOffset::clamped(offset),
                held,
                index: results.len(),
            });
        }
        ProbeStep::Done(Found {
            members: found,
            stopped: false,
        })
    }
}

/// Whether some member's offset is already deeper than the profile holds it: the profile cannot
/// reach the combination, so no backoff or probing is needed.
pub(crate) fn already_broken(members: &PerCore<TargetOffset>, profile: &Profile) -> bool {
    members
        .iter()
        .any(|(core, offset)| profile.at(core).shallower_than(offset))
}

/// The move a combination asks of the tuner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Backoff {
    /// Move `member` to the offset of `passed[probe]`, a passing member probe.
    Tested {
        probe: usize,
        member: CoreId,
        to: TargetOffset,
    },
    /// No tested probe applies: move `member` one count past the combination, which leaves the
    /// most depth, `reach` counts, reachable.
    Untested {
        member: CoreId,
        to: TargetOffset,
        reach: i32,
    },
}

/// Chooses the combination backoff. Among the passing probes that moved only their member (every
/// other member at its failing offset), move the current profile shallower and leave it reaching
/// no constraint, the one moving its member the fewest counts wins, ties going to the lowest-ranked
/// member. Without one, the member whose move one count past the combination leaves the most depth
/// reachable, again ties to the lowest-ranked member. `constraints` already holds the combination.
pub(crate) fn combination_backoff(
    members: &PerCore<TargetOffset>,
    passed: &[Probe],
    failing: &Profile,
    current: &Profile,
    constraints: &Constraints,
    ranking: &Ranking,
) -> Option<Backoff> {
    let rank = |core: CoreId| {
        ranking
            .order()
            .iter()
            .position(|&c| c == core)
            .unwrap_or(usize::MAX)
    };
    let mut tested: Option<(usize, i64, usize)> = None;
    for (i, probe) in passed.iter().enumerate() {
        let at = current.at(probe.member);
        if !probe.holds_failing(failing) || !probe.offset.shallower_than(at) {
            continue;
        }
        if !matches!(
            constraints.reaches(&current.with(probe.member, probe.offset)),
            Ok(None)
        ) {
            continue;
        }
        let (moved, order) = (probe.offset.get() - at.get(), rank(probe.member));
        if tested.is_none_or(|(_, best, best_order)| {
            moved < best || (moved == best && order > best_order)
        }) {
            tested = Some((i, moved, order));
        }
    }
    if let Some((i, _, _)) = tested {
        let probe = passed.get(i)?;
        return Some(Backoff::Tested {
            probe: i,
            member: probe.member,
            to: probe.offset,
        });
    }
    let mut untested: Option<(CoreId, TargetOffset, i32, usize)> = None;
    for (member, offset) in members.iter() {
        let past = offset.shallower_by(1);
        let at = current.at(member);
        let to = if at.shallower_than(past) { at } else { past };
        let Ok(Some(target)) = constraints.optimum(&current.with(member, to), ranking) else {
            continue;
        };
        let (total, order) = (target.total(), rank(member));
        if untested.is_none_or(|(_, _, best, best_order)| {
            total < -best || (total == -best && order > best_order)
        }) {
            untested = Some((member, to, -total, order));
        }
    }
    untested.map(|(member, to, reach, _)| Backoff::Untested { member, to, reach })
}
