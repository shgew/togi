use std::num::{NonZeroU8, NonZeroU32};

use serde::{Deserialize, Serialize};
use togi_journal::{Activity, HuntStage, Member, Seq, TrialId};
use togi_machine::{CoreId, CoreSet, PerCore, Regime, TargetOffset, WorkloadId};

use crate::hunt::plan::GroupOutcome;
use crate::search::SearchRegime;
use crate::state::Run;
use crate::{Limit, Profile, Requirement, State, TrialContext, TrialHistory, Window};

mod wire;

/// Trials of one length: how many, and how long each runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Quota {
    pub count: u32,
    pub secs: u32,
}

/// The trial lengths of a schedule: its first, and a longer one after it when it has one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lengths {
    pub short: Quota,
    pub long: Option<Quota>,
}

impl Lengths {
    pub fn iter(self) -> impl Iterator<Item = Quota> {
        std::iter::once(self.short).chain(self.long)
    }

    pub fn trials(self) -> u32 {
        self.iter().map(|quota| quota.count).sum()
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts of one part shown as the dashboard shows them"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(into = "wire::CyclePart", from = "wire::CyclePart")]
pub struct CyclePart {
    pub cores: CoreSet,
    pub ccd: Option<togi_machine::CcdId>,
    pub full: bool,
    pub partial: bool,
    /// The part's quotas with trials to run, none when it has none.
    pub lengths: Option<Lengths>,
    /// Credited passes: each quota's valid passes clipped to that quota's count, so surplus
    /// evidence in one class never fills another. Raw evidence stays in the evidence counts.
    pub passed: u32,
    pub failed: u32,
    pub done: bool,
    pub running: bool,
}

impl CyclePart {
    /// The part's short and long trial counts, zero for a length it lacks.
    pub fn counts(&self) -> (u32, u32) {
        self.lengths.map_or((0, 0), |lengths| {
            (
                lengths.short.count,
                lengths.long.map_or(0, |long| long.count),
            )
        })
    }

    pub fn trials(&self) -> u32 {
        self.lengths.map_or(0, Lengths::trials)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CycleStep {
    pub regime: Regime,
    pub workload: WorkloadId,
    pub parts: Vec<CyclePart>,
    pub chains_complete: bool,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CyclePlan {
    pub number: Option<NonZeroU32>,
    pub open: bool,
    pub steps: Vec<CycleStep>,
    pub current: usize,
    pub paused: bool,
}

/// What a search turn does: confirm the candidate solo limit, or search deeper by `step` counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnKind {
    Confirm,
    Search { step: NonZeroU8 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(into = "wire::SearchTurn", try_from = "wire::SearchTurn")]
pub struct SearchTurn {
    pub core: CoreId,
    pub kind: TurnKind,
    pub regimes: Vec<Regime>,
    pub offset: TargetOffset,
    pub workload: WorkloadId,
    pub light: u32,
    pub heavy: u32,
    pub needed: u32,
    pub running: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrialRequirement {
    pub passed: u32,
    pub failed: u32,
    pub trial: u32,
    pub needed: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupState {
    Running,
    Passed,
    Failed,
    Skipped,
}

impl GroupState {
    pub(crate) const fn of(outcome: Option<GroupOutcome>) -> Self {
        match outcome {
            None => Self::Running,
            Some(GroupOutcome::Passed) => Self::Passed,
            Some(GroupOutcome::Failed) => Self::Failed,
            Some(GroupOutcome::Skipped) => Self::Skipped,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HuntPart {
    pub failing: CoreSet,
    pub parked: CoreSet,
    pub trials: u32,
    pub duration_s: u32,
    pub group: Option<u32>,
    pub outcome: Option<GroupState>,
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HuntGroup {
    pub number: u32,
    pub cores: CoreSet,
    pub profile: PerCore<TargetOffset>,
    pub stage: HuntStage,
    pub probe: Option<Member>,
    pub held: Vec<Member>,
    pub outcome: GroupState,
    pub passed: u32,
    pub needed: u32,
    pub carried: bool,
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemberProbe {
    pub member: CoreId,
    pub offset: TargetOffset,
    pub failed_at: Vec<TargetOffset>,
    pub passed_at: Vec<TargetOffset>,
    pub carried: Vec<TargetOffset>,
    pub running: bool,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(into = "wire::RerunPlan", try_from = "wire::RerunPlan")]
pub struct RerunPlan {
    pub regime: Regime,
    pub cores: CoreSet,
    pub lengths: Lengths,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HuntPlan {
    pub number: u32,
    pub regime: Regime,
    pub failure: Seq,
    pub trial: Option<TrialId>,
    pub candidates: CoreSet,
    pub cut: u32,
    pub parts: Vec<HuntPart>,
    pub groups: Vec<HuntGroup>,
    pub probes: Vec<MemberProbe>,
    pub rerun: RerunPlan,
}

impl State {
    fn live(&self) -> Option<&Run> {
        if self.poisoned() { None } else { self.run() }
    }

    pub fn core_limit(&self, core: CoreId) -> Option<Limit> {
        let run = self.live()?;
        if !run.topo.contains(core) {
            return None;
        }
        run.constraints
            .at_limit(core, &run.offsets())
            .ok()
            .flatten()
    }

    pub fn cycle_plan(&self) -> CyclePlan {
        self.live().map_or_else(
            || CyclePlan {
                number: None,
                open: false,
                steps: Vec::new(),
                current: 0,
                paused: false,
            },
            Run::cycle_plan,
        )
    }

    pub fn search_turns(&self) -> Vec<SearchTurn> {
        self.live().map(Run::search_turns).unwrap_or_default()
    }

    pub fn requirement(&self) -> Option<TrialRequirement> {
        self.live()?.trial_requirement()
    }

    pub fn trial_history(&self) -> Option<TrialHistory> {
        self.live()?
            .flight
            .last_closed
            .as_ref()
            .map(|closed| closed.history)
    }

    /// The requirement the open trial was scheduled for, as its `trial.intent` folded.
    pub fn open_requirement(&self) -> Option<Requirement> {
        self.live()?.flight.open().map(|open| open.requirement)
    }

    pub fn hunt_plan(&self) -> Option<HuntPlan> {
        self.live()?.hunt_plan()
    }
}

impl Run {
    fn cycle_plan(&self) -> CyclePlan {
        let paused = self.hunts.active.is_some() || !self.reruns.is_empty();
        let closed = self.checking.closed();
        let (cycle, open) = match (self.checking.open(), closed) {
            (Some(cycle), _) => (cycle, true),
            (None, Some(closed)) => (&closed.cycle, false),
            (None, None) => {
                return CyclePlan {
                    number: self.checking.last_cycle(),
                    open: false,
                    steps: Vec::new(),
                    current: 0,
                    paused,
                };
            }
        };
        let finished =
            !open && closed.is_some_and(|closed| closed.steps_done == cycle.plan().len());
        let running = self
            .running_slot(cycle.number)
            .map(|(trial, step, part)| (trial, (step, part)));
        let pinned = running.and_then(|(trial, _)| self.checking.pinned(trial));
        let checking = self.checking.profile();
        let steps: Vec<CycleStep> = cycle
            .plan()
            .iter()
            .enumerate()
            .map(|(index, step)| {
                let parts: Vec<CyclePart> = step
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(at, planned)| {
                        let is_running = running.is_some_and(|(_, slot)| slot == (index, at));
                        let stored = pinned.filter(|_| is_running);
                        let part = stored.map_or(planned, |pinned| &pinned.plan);
                        let ccd = part
                            .cores
                            .iter()
                            .map(|core| self.topo.ccd_of(core))
                            .reduce(|a, b| if a == b { a } else { None })
                            .flatten();
                        let size = ccd.map_or(self.topo.all().len(), |ccd| {
                            self.topo
                                .ccds()
                                .iter()
                                .find(|(id, _)| *id == ccd)
                                .map_or(0, |(_, cores)| cores.len())
                        });
                        let full = part.cores.len() == size;
                        let mut view = CyclePart {
                            cores: part.cores,
                            ccd,
                            full,
                            partial: step.regime == Regime::R7 && !full,
                            lengths: None,
                            passed: 0,
                            failed: 0,
                            done: true,
                            running: is_running,
                        };
                        for quota in part.quotas.iter().filter(|quota| quota.count > 0) {
                            let length = Quota {
                                count: quota.count,
                                secs: quota.class.duration().get(),
                            };
                            match &mut view.lengths {
                                None => {
                                    view.lengths = Some(Lengths {
                                        short: length,
                                        long: None,
                                    });
                                }
                                Some(lengths) => lengths.long = Some(length),
                            }
                            let passed = if stored.is_some() {
                                self.cycle_count(cycle, quota.class)
                            } else {
                                cycle.have(quota.class)
                            };
                            view.passed += passed.min(quota.count);
                            view.failed += checking.map_or(0, |profile| {
                                self.evidence
                                    .live_failures(quota.class, &profile, Some(cycle.start))
                                    .unwrap_or(0)
                            });
                            view.done &= passed >= quota.count;
                        }
                        view.done |= finished;
                        view
                    })
                    .collect();
                let chains_complete = finished || cycle.chains_complete(index, &self.topo);
                let done = chains_complete && parts.iter().all(|part| part.done);
                CycleStep {
                    regime: step.regime,
                    workload: step.workload,
                    parts,
                    chains_complete,
                    done,
                }
            })
            .collect();
        let current = steps
            .iter()
            .position(|step| !step.done)
            .unwrap_or(steps.len());
        CyclePlan {
            number: Some(cycle.number),
            open,
            steps,
            current,
            paused,
        }
    }

    fn trial_requirement(&self) -> Option<TrialRequirement> {
        let open = self.flight.open()?;
        let history = self.history(open);
        let failed = if let TrialContext::Cycle { .. } = open.requirement.context {
            let (Window::Live { since } | Window::LiveOrCarried { since }) =
                open.requirement.window;
            self.evidence
                .live_failures(open.requirement.class, &open.profile, since)
                .unwrap_or(0)
        } else {
            0
        };
        Some(TrialRequirement {
            passed: history.passed,
            failed,
            trial: history.trial,
            needed: history.needed,
        })
    }

    fn search_turns(&self) -> Vec<SearchTurn> {
        self.search_order()
            .map(|id| self.turn_view(id, self.search_regime(id)))
            .collect()
    }

    fn turn_view(&self, id: CoreId, search_regime: SearchRegime) -> SearchTurn {
        let core = self.core(id);
        let needed = self.config.needed;
        let (light, heavy, regime, frozen) = match core.phase.check() {
            Some(check) => {
                let profile = Profile::alone(&self.topo, id, core.offset);
                let window = Window::LiveOrCarried {
                    since: Some(check.since),
                };
                let valid = |frozen| {
                    self.evidence
                        .count(self.alone_class(id, frozen), &profile, window, needed)
                        .map_or(0, |count| count.valid)
                };
                let light = valid(check.r1);
                let heavy = valid(check.r2);
                if light >= u32::from(needed.get()) {
                    (light, heavy, SearchRegime::R2, Some(check.r2))
                } else {
                    (light, heavy, SearchRegime::R1, Some(check.r1))
                }
            }
            None => (0, 0, search_regime, None),
        };
        let kind = if core.phase.check().is_some() {
            TurnKind::Confirm
        } else if self.failure_point(id).is_some() {
            TurnKind::Search {
                step: NonZeroU8::MIN,
            }
        } else {
            TurnKind::Search {
                step: NonZeroU8::MIN.saturating_add(4),
            }
        };
        let regimes = match (core.phase.check(), search_regime) {
            (None, SearchRegime::R2) => vec![Regime::R2],
            (None, SearchRegime::R1) | (Some(_), SearchRegime::R1 | SearchRegime::R2) => {
                vec![Regime::R1, Regime::R2]
            }
        };
        let plan = self.search_trial(id, regime, frozen);
        SearchTurn {
            core: id,
            kind,
            regimes,
            offset: core.offset,
            workload: plan.workload_at(self.rotation(&plan)),
            light,
            heavy,
            needed: u32::from(needed.get()),
            running: self.flight.open().is_some_and(|open| {
                open.intent.core == Some(id) && open.intent.activity == Some(Activity::Search)
            }),
        }
    }
}
