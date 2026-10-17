use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use togi_journal::{Activity, HuntStage, Member, Seq, TrialId};
use togi_machine::{CoreId, CoreSet, PerCore, Regime, TargetOffset, WorkloadId};

use crate::hunt::plan::GroupOutcome;
use crate::state::Run;
use crate::{Limit, Profile, State, TrialContext, TrialHistory, Window};

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts of one part shown as the dashboard shows them"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CyclePart {
    pub cores: CoreSet,
    pub ccd: Option<togi_machine::CcdId>,
    pub full: bool,
    pub partial: bool,
    pub short: u32,
    pub short_s: u32,
    pub long: u32,
    pub long_s: u32,
    /// Credited passes: each quota's valid passes clipped to that quota's count, so surplus
    /// evidence in one class never fills another. Raw evidence stays in the evidence counts.
    pub passed: u32,
    pub failed: u32,
    pub done: bool,
    pub running: bool,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchTurn {
    pub core: CoreId,
    pub confirm: bool,
    pub regimes: Vec<Regime>,
    pub offset: TargetOffset,
    pub workload: WorkloadId,
    pub step: u8,
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
pub struct RerunPlan {
    pub regime: Regime,
    pub cores: CoreSet,
    pub short: u32,
    pub short_s: u32,
    pub long: u32,
    pub long_s: u32,
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
                            short: 0,
                            short_s: 0,
                            long: 0,
                            long_s: 0,
                            passed: 0,
                            failed: 0,
                            done: true,
                            running: is_running,
                        };
                        for quota in part.quotas.iter().filter(|quota| quota.count > 0) {
                            let secs = quota.class.duration().get();
                            if view.short_s == 0 {
                                view.short = quota.count;
                                view.short_s = secs;
                            } else {
                                view.long = quota.count;
                                view.long_s = secs;
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
        let order = self.topo.order();
        let cursor = self.search.cursor();
        let current = cursor.and_then(|at| order.get(at).copied());
        let second_only = |id: CoreId| {
            self.searching(id)
                .is_some_and(|core| core.phase.check().is_none())
                && matches!(self.search.current(id, &self.evidence), (Some(_), None))
        };
        let mut ids: Vec<CoreId> = current.filter(|&id| second_only(id)).into_iter().collect();
        let start = cursor.map_or(0, |at| at + 1);
        for step in 0..order.len() {
            let id = order[(start + step) % order.len()];
            if self.searching(id).is_some() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.into_iter()
            .map(|id| self.turn_view(id, second_only(id)))
            .collect()
    }

    fn turn_view(&self, id: CoreId, second_only: bool) -> SearchTurn {
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
                    (light, heavy, Regime::R2, Some(check.r2))
                } else {
                    (light, heavy, Regime::R1, Some(check.r1))
                }
            }
            None if second_only => (0, 0, Regime::R2, None),
            None => (0, 0, Regime::R1, None),
        };
        let step = if core.phase.check().is_some() {
            0
        } else if self.failure_point(id).is_some() {
            1
        } else {
            5
        };
        let regimes = if core.phase.check().is_none() && second_only {
            vec![Regime::R2]
        } else {
            vec![Regime::R1, Regime::R2]
        };
        let plan = self.search_trial(id, regime, frozen);
        SearchTurn {
            core: id,
            confirm: core.phase.check().is_some(),
            regimes,
            offset: core.offset,
            workload: plan.workload_at(self.rotation(&plan)),
            step,
            light,
            heavy,
            needed: u32::from(needed.get()),
            running: self.flight.open().is_some_and(|open| {
                open.intent.core == Some(id) && open.intent.activity == Some(Activity::Search)
            }),
        }
    }
}
