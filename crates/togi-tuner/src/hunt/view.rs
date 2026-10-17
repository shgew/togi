use togi_journal::{HuntStage, Member, Seq};
use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use super::next::{Next, stage_of};
use super::plan::{
    End, Found, Group, GroupOutcome, GroupStage, MemberProbes, ProbeResult, ProbeStep,
    already_broken, partition,
};
use super::state::{Active, GroupRecord};
use crate::Profile;
use crate::dashboard::{
    GroupState, HuntGroup, HuntPart, HuntPlan, Lengths, MemberProbe, Quota, RerunPlan,
};
use crate::state::Run;

/// One group of the active hunt as a dashboard shows it, its outcome read from the answer the
/// hunt froze when it moved on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupView {
    pub(crate) number: u32,
    pub(crate) seq: Seq,
    pub(crate) cores: CoreSet,
    pub(crate) profile: Profile,
    pub(crate) stage: HuntStage,
    pub(crate) probe: Option<Member>,
    pub(crate) held: Vec<Member>,
    pub(crate) outcome: Option<GroupOutcome>,
    pub(crate) needed: u16,
    pub(crate) passed: u32,
    pub(crate) carried: bool,
    pub(crate) running: bool,
}

impl Run {
    pub(crate) fn hunt_groups(&self) -> Vec<GroupView> {
        let Some(hunt) = self.hunts.active.as_ref() else {
            return Vec::new();
        };
        hunt.groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                let answer = if hunt.settled(index) {
                    group.closing.clone().unwrap_or_default()
                } else {
                    self.answer_of(hunt, group)
                };
                let own = if group.payload.inferred.is_some() {
                    group.own.clone().unwrap_or_default()
                } else {
                    answer.clone()
                };
                let outcome = if group.payload.skipped {
                    Some(GroupOutcome::Skipped)
                } else {
                    answer.outcome
                };
                GroupView {
                    number: group.number(),
                    seq: group.seq,
                    cores: group.payload.cores,
                    profile: group.profile,
                    stage: group.payload.stage,
                    probe: group.payload.probe,
                    held: group.payload.held.clone(),
                    outcome,
                    needed: hunt.needed.get(),
                    passed: if hunt.settled(index) {
                        answer.passes
                    } else {
                        self.passes_of(hunt, group)
                    },
                    carried: own.is_carried(|seq| self.is_carried(seq)),
                    running: self.group_running(hunt, group),
                }
            })
            .collect()
    }

    fn group_running(&self, hunt: &Active, group: &GroupRecord) -> bool {
        self.flight.open().is_some_and(|open| {
            open.intent.hunt == Some(hunt.id) && open.intent.group == Some(group.payload.group)
        })
    }

    pub(crate) fn hunt_cut(&self) -> u32 {
        self.hunts.active.as_ref().map_or(0, |hunt| hunt.cut.count)
    }
}

/// What the active hunt's plan shows: the group in flight or up next, or the end it reaches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shape {
    Group(GroupShape),
    End(End),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GroupShape {
    stage: HuntStage,
    set: CoreSet,
    cores: CoreSet,
    granularity: u32,
    duration_s: u32,
    repeated: bool,
}

impl Shape {
    fn recorded(hunt: &Active, group: &GroupRecord) -> Self {
        let payload = &group.payload;
        Self::Group(GroupShape {
            stage: payload.stage,
            set: payload.set,
            cores: payload.cores,
            granularity: payload.granularity,
            duration_s: payload.duration_s,
            repeated: is_repeated(hunt, group),
        })
    }

    fn upcoming(hunt: &Active, group: &Group) -> Self {
        Self::Group(GroupShape {
            stage: stage_of(group.stage),
            set: group.set,
            cores: group.cores,
            granularity: u32::try_from(group.granularity).unwrap_or(u32::MAX),
            duration_s: Run::hunt_secs(hunt, group.duration).get(),
            repeated: group.stage == GroupStage::Repeated,
        })
    }

    /// The cores the group splits, or the cores the end settles on.
    fn set(self) -> CoreSet {
        match self {
            Self::Group(group) => group.set,
            Self::End(End::Culprit(core)) => std::iter::once(core).collect(),
            Self::End(End::Combination(set) | End::Fallback(set)) => set,
            Self::End(End::Loaded) => CoreSet::default(),
        }
    }
}

fn is_repeated(hunt: &Active, group: &GroupRecord) -> bool {
    let payload = &group.payload;
    payload.stage == HuntStage::Part
        && payload.cores.len() == 1
        && payload.granularity > 2
        && u32::try_from(payload.set.len()) == Ok(payload.granularity)
        && payload.set == hunt.start.candidates
        && hunt
            .groups
            .iter()
            .find(|other| other.payload.stage != HuntStage::Locate)
            .is_some_and(|first| first.payload.group == payload.group)
}

impl Run {
    fn hunt_shape(&self, hunt: &Active) -> Shape {
        if let Some(latest) = hunt.latest() {
            if latest.is_probe() || self.answer_of(hunt, latest).outcome.is_none() {
                return Shape::recorded(hunt, latest);
            }
        }
        match self.plan_next(hunt) {
            Next::Group(group) => Shape::upcoming(hunt, &group),
            Next::End(end) => Shape::End(end),
        }
    }

    pub(crate) fn hunt_plan(&self) -> Option<HuntPlan> {
        let hunt = self.hunts.active.as_deref()?;
        let shape = self.hunt_shape(hunt);
        let views = self.hunt_groups();
        let probing = hunt.groups.iter().any(GroupRecord::is_probe)
            || matches!(shape, Shape::End(End::Combination(_)))
            || matches!(shape, Shape::End(End::Culprit(core)) if self.singleton_probing(hunt, core).is_some());
        Some(HuntPlan {
            number: hunt.id.get(),
            regime: hunt.start.regime,
            failure: hunt.start.failure,
            trial: hunt.start.trial.clone(),
            candidates: hunt.start.candidates,
            cut: self.hunt_plan_cut(hunt, shape),
            parts: self.hunt_parts(hunt, shape, &views),
            probes: if probing {
                self.hunt_probes(hunt, shape, &views)
            } else {
                Vec::new()
            },
            groups: views.iter().map(HuntGroup::from).collect(),
            rerun: self.hunt_rerun(hunt),
        })
    }

    fn hunt_plan_cut(&self, hunt: &Active, shape: Shape) -> u32 {
        let Shape::Group(
            shape @ GroupShape {
                stage: HuntStage::Part | HuntStage::Complement,
                ..
            },
        ) = shape
        else {
            return 0;
        };
        let signature = (shape.set, shape.granularity, shape.duration_s);
        let count = self.hunt_cut();
        if hunt.cut.signature == Some(signature) {
            count
        } else {
            count.saturating_add(1)
        }
    }

    fn hunt_rerun(&self, hunt: &Active) -> RerunPlan {
        let short = self.config.short_trial;
        let long = hunt.class.duration();
        RerunPlan {
            regime: hunt.class.regime(),
            cores: hunt.class.loaded(),
            lengths: Lengths {
                short: Quota {
                    count: u32::from(self.config.needed.get()),
                    secs: short.get(),
                },
                long: (long != short).then_some(Quota {
                    count: 1,
                    secs: long.get(),
                }),
            },
        }
    }

    fn hunt_parts(&self, hunt: &Active, shape: Shape, views: &[GroupView]) -> Vec<HuntPart> {
        let Shape::Group(shape) = shape else {
            return Vec::new();
        };
        let part = |failing: CoreSet, group: Option<usize>| {
            let view = group.and_then(|index| views.get(index));
            HuntPart {
                failing,
                parked: self
                    .topo
                    .all()
                    .iter()
                    .filter(|&core| !failing.contains(core))
                    .collect(),
                trials: hunt.start.trials,
                duration_s: shape.duration_s,
                group: view.map(|view| view.number),
                outcome: view.map(|view| GroupState::of(view.outcome)),
                running: view.is_some_and(|view| view.running),
            }
        };
        if shape.repeated {
            let group = hunt
                .groups
                .iter()
                .position(|group| is_repeated(hunt, group));
            return vec![part(shape.cores, group)];
        }
        match shape.stage {
            stage @ (HuntStage::Part | HuntStage::Complement) => {
                let split = (shape.set == hunt.start.candidates).then(|| hunt.class.loaded());
                let granularity = usize::try_from(shape.granularity).unwrap_or(usize::MAX);
                partition(shape.set, granularity, split, hunt.recent)
                    .iter()
                    .enumerate()
                    .map(|(index, piece)| {
                        let failing = if stage == HuntStage::Part {
                            *piece
                        } else {
                            shape
                                .set
                                .iter()
                                .filter(|&core| !piece.contains(core))
                                .collect()
                        };
                        let group = hunt.groups.iter().rposition(|group| {
                            let recorded = &group.payload;
                            recorded.stage == stage
                                && recorded.granularity == shape.granularity
                                && recorded.duration_s == shape.duration_s
                                && recorded.set == shape.set
                                && u32::try_from(index) == Ok(recorded.index)
                                && recorded.probe.is_none()
                        });
                        part(failing, group)
                    })
                    .collect()
            }
            HuntStage::Full => {
                let group = hunt
                    .groups
                    .len()
                    .checked_sub(1)
                    .filter(|&last| hunt.groups[last].payload.stage == HuntStage::Full);
                vec![part(shape.set, group)]
            }
            HuntStage::Locate | HuntStage::Probe => Vec::new(),
        }
    }

    fn probe_step(&self, hunt: &Active, set: CoreSet) -> Option<ProbeStep> {
        if set.len() == 1 {
            if hunt.latest().is_some_and(|group| {
                group.is_probe() && self.answer_of(hunt, group).outcome.is_none()
            }) {
                return None;
            }
            return self.singleton_probing(hunt, set.first()?);
        }
        let checking = self.checking.profile()?;
        let failing: PerCore<TargetOffset> = hunt.failing.among(set).collect();
        if already_broken(&failing, &checking) {
            return Some(ProbeStep::Done(Found {
                members: failing,
                stopped: false,
            }));
        }
        let results: Vec<ProbeResult> = hunt
            .groups
            .iter()
            .filter_map(|group| {
                let probe = group.payload.probe?;
                Some(ProbeResult {
                    member: probe.core,
                    offset: probe.offset,
                    outcome: self.answer_of(hunt, group).outcome?,
                })
            })
            .collect();
        Some(MemberProbes::new(set, &hunt.failing, &hunt.parked).step(&results))
    }

    fn hunt_probes(&self, hunt: &Active, shape: Shape, views: &[GroupView]) -> Vec<MemberProbe> {
        let set = shape.set();
        let step = self.probe_step(hunt, set);
        hunt.failing
            .among(set)
            .map(|(core, origin)| {
                let mut probe = member_ladder(hunt, core, origin, views);
                match step {
                    Some(ProbeStep::Run(next)) => {
                        if next.member == core && !probe.running {
                            probe.offset = next.offset;
                        }
                        if core < next.member {
                            probe.done = true;
                            probe.offset = next.held.get(core).unwrap_or(probe.offset);
                        }
                    }
                    Some(ProbeStep::Done(found)) => {
                        probe.done = true;
                        probe.offset = found.members.get(core).unwrap_or(probe.offset);
                    }
                    None => {}
                }
                probe
            })
            .collect()
    }
}

fn member_ladder(
    hunt: &Active,
    core: CoreId,
    origin: TargetOffset,
    views: &[GroupView],
) -> MemberProbe {
    let mut probe = MemberProbe {
        member: core,
        offset: origin,
        failed_at: vec![origin],
        passed_at: Vec::new(),
        carried: Vec::new(),
        running: false,
        done: false,
    };
    for (group, view) in hunt.groups.iter().zip(views) {
        let Some(member) = group.payload.probe.filter(|member| member.core == core) else {
            continue;
        };
        probe.offset = member.offset;
        match view.outcome {
            Some(GroupOutcome::Passed) => {
                probe.passed_at.push(member.offset);
                if view.carried {
                    probe.carried.push(member.offset);
                }
            }
            Some(GroupOutcome::Failed) => probe.failed_at.push(member.offset),
            Some(GroupOutcome::Skipped) | None => {}
        }
        probe.running = view.running;
    }
    probe
}

impl From<&GroupView> for HuntGroup {
    fn from(view: &GroupView) -> Self {
        Self {
            number: view.number,
            cores: view.cores,
            profile: view.profile.offsets(),
            stage: view.stage,
            probe: view.probe,
            held: view.held.clone(),
            outcome: GroupState::of(view.outcome),
            passed: view.passed,
            needed: u32::from(view.needed),
            carried: view.carried,
            running: view.running,
        }
    }
}
