use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{
    Activity, CheckingChain, CheckingCycle, CheckingStep, CycleEvent, Rederived, Seq, TrialIntent,
};
use togi_machine::{Condition, CoreSet};

use super::schedule::{self, Quota};
use super::{Link, OpenCycle};
use crate::failures::load_of;
use crate::r7::order::RequestOrder;
use crate::state::Run;
use crate::{
    Action, ClassKey, DecisionAction, DecisionPayload, LoadKey, PartNo, Requirement, StepNo,
    TrialContext, TrialPlan, Window, WorkloadChoice,
};

fn ordinal(index: usize) -> NonZeroU32 {
    NonZeroU32::new(u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1))
        .unwrap_or(NonZeroU32::MAX)
}

fn joined(parts: impl IntoIterator<Item = String>) -> Option<String> {
    let text = parts.into_iter().collect::<Vec<_>>().join("; ");
    (!text.is_empty()).then_some(text)
}

fn distinct(seqs: impl IntoIterator<Item = Seq>) -> Vec<Seq> {
    let mut out: Vec<Seq> = Vec::new();
    for seq in seqs {
        if !out.contains(&seq) {
            out.push(seq);
        }
    }
    out
}

impl Run {
    /// Whether `intent` runs `class` together on the checking profile, loading the cores the
    /// class names the way the tuner's own plans do.
    pub(crate) fn together_shape(intent: &TrialIntent, class: ClassKey) -> bool {
        let load_matches = match load_of(class) {
            crate::Load::Single(core) => intent.core == Some(core) && intent.cores.is_empty(),
            crate::Load::Many(cores) => intent.core.is_none() && intent.cores == cores,
        };
        load_matches
            && intent.condition == Condition::Together
            && intent.activity == Some(Activity::Checking)
            && intent.hunt.is_none()
            && intent.group.is_none()
            && intent.round.is_none()
    }

    fn cycle_requirement(open: &OpenCycle, step: usize, part: usize, quota: &Quota) -> Requirement {
        Requirement {
            context: TrialContext::Cycle {
                cycle: open.number,
                step: StepNo::new(ordinal(step)),
                part: PartNo::new(ordinal(part)),
            },
            class: quota.class,
            window: Window::Live {
                since: Some(open.start),
            },
            needed: NonZeroU16::new(u16::try_from(quota.count).unwrap_or(u16::MAX))
                .unwrap_or(NonZeroU16::MIN),
        }
    }

    /// The requirement `class` has in the open cycle's step `step`, and the part that holds it.
    fn quota_in_step(open: &OpenCycle, step: usize, class: ClassKey) -> Option<(usize, &Quota)> {
        let plan = open.plan.get(step)?;
        if plan.regime == togi_machine::Regime::R7 && !open.r7.contains_key(&step) {
            return None;
        }
        plan.parts.iter().enumerate().find_map(|(at, part)| {
            part.quotas
                .iter()
                .find(|quota| quota.class == class && quota.count > 0)
                .map(|quota| (at, quota))
        })
    }

    /// The requirement of a checking trial intent when it is a trial of the open cycle's step.
    pub(crate) fn cycle_context(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
    ) -> Option<Requirement> {
        let open = self.checking.open()?;
        let step = usize::try_from(intent.step?.get() - 1).ok()?;
        let matches = intent.cycle == Some(open.number)
            && !intent.rerun
            && Self::together_shape(intent, class);
        let (part, quota) = Self::quota_in_step(open, step, class).filter(|_| matches)?;
        Some(Self::cycle_requirement(open, step, part, quota))
    }

    /// The requirement a saved cycle retry would carry now, when its cycle is open and its step
    /// still holds its class.
    pub(crate) fn cycle_retry_requirement(&self, plan: &TrialPlan) -> Option<Requirement> {
        let TrialContext::Cycle { cycle, step, .. } = plan.requirement.context else {
            return None;
        };
        let open = self.checking.open().filter(|open| open.number == cycle)?;
        let WorkloadChoice::Frozen(workload) = plan.workload else {
            return None;
        };
        let class = ClassKey::new(workload, plan.load.cores(), plan.duration);
        let step = usize::try_from(step.get().get() - 1).ok()?;
        let (part, quota) = Self::quota_in_step(open, step, class)?;
        Some(Self::cycle_requirement(open, step, part, quota))
    }

    fn cycle_trial(
        &self,
        open: &OpenCycle,
        step: usize,
        part: usize,
        quota: &Quota,
    ) -> Option<Action> {
        let profile = self.checking.profile()?;
        let requirement = Self::cycle_requirement(open, step, part, quota);
        let cause: Vec<Seq> = self.checking.last_seq().into_iter().collect();
        if let Some(plan) = self.serve_checking_retry(&requirement, cause.clone()) {
            return Some(Action::RunTrial(plan));
        }
        let class = quota.class;
        Some(Action::RunTrial(TrialPlan {
            regime: class.regime(),
            workload: WorkloadChoice::Frozen(class.workload()),
            load: load_of(class),
            condition: Condition::Together,
            activity: Activity::Checking,
            profile,
            duration: class.duration(),
            requirement,
            retry: false,
            cause,
        }))
    }

    fn unmet_trial(&self, open: &OpenCycle, step: usize, part: usize) -> Option<Action> {
        let plan = open.plan.get(step)?.parts.get(part)?;
        plan.quotas
            .iter()
            .find(|quota| !open.met(quota))
            .and_then(|quota| self.cycle_trial(open, step, part, quota))
    }

    fn derive_chain(
        &self,
        open: &OpenCycle,
        step: usize,
        ccd: togi_machine::CcdId,
        previous: CoreSet,
    ) -> Option<Action> {
        let profile = self.checking.profile()?;
        let started = open.r7.get(&step)?;
        let workload = open.plan.get(step)?.workload;
        let index = started.current(ccd).len();
        let order = RequestOrder::derive(
            &self.r7.measurements,
            &self.topo,
            LoadKey::new(workload, previous),
            &profile,
        );
        let groups: Vec<CoreSet> = order
            .ccd(ccd)
            .map(|order| order.groups().to_vec())
            .unwrap_or_default();
        let top = groups.first().copied().unwrap_or_default();
        let rest: CoreSet = previous
            .iter()
            .filter(|&core| !top.contains(core))
            .collect();
        let link = if rest.len() < 2 {
            Link::End
        } else {
            Link::Partial(rest)
        };
        let rederived = started
            .chain(ccd)
            .and_then(|chain| chain.recorded(index))
            .map(|old| {
                if old.link == link {
                    Rederived::Unchanged
                } else {
                    Rederived::Changed
                }
            });
        let cause = distinct(
            std::iter::once(started.start)
                .chain(self.checking.last_seq())
                .chain(order.sources().iter().copied()),
        );
        Some(Action::Decide(DecisionAction::new(
            CheckingChain {
                cycle: open.number,
                step: ordinal(step),
                ccd,
                workload,
                groups,
                cores: link.recorded_cores(),
                source_seqs: order.sources().to_vec(),
                profile: profile.offsets(),
                part: ordinal(index),
                rederived,
            },
            cause,
        )))
    }

    fn r7_step_action(&self, open: &OpenCycle, step: usize) -> Option<Action> {
        let Some(started) = open.r7.get(&step) else {
            let profile = self.checking.profile()?;
            return Some(Action::Decide(DecisionAction::new(
                CheckingStep {
                    cycle: open.number,
                    step: ordinal(step),
                    profile: profile.offsets(),
                },
                self.checking.last_seq().into_iter().collect(),
            )));
        };
        let plan = open.plan.get(step)?;
        let mut at = 0;
        for &(ccd, _) in self.topo.ccds() {
            let mut last = at;
            while plan.parts.get(at).is_some_and(|part| part.ccd == Some(ccd)) {
                if let Some(trial) = self.unmet_trial(open, step, at) {
                    return Some(trial);
                }
                last = at;
                at += 1;
            }
            if !started
                .chain(ccd)
                .is_some_and(super::chains::Chain::complete)
            {
                return self.derive_chain(open, step, ccd, plan.parts.get(last)?.cores);
            }
        }
        if self.topo.ccds().len() > 1 {
            return self.unmet_trial(open, step, plan.parts.len().checked_sub(1)?);
        }
        None
    }

    /// The first unmet requirement of the first unfinished step, the trial that would meet it,
    /// or the step, chain or cycle-end decision in its way.
    pub(crate) fn cycle_action(&self) -> Option<Action> {
        let open = self.checking.open()?;
        for step in open.steps_done()..open.plan.len() {
            let plan = &open.plan[step];
            let found = if plan.regime == togi_machine::Regime::R7 {
                self.r7_step_action(open, step)
            } else {
                (0..plan.parts.len()).find_map(|part| self.unmet_trial(open, step, part))
            };
            if found.is_some() {
                return found;
            }
        }
        Some(self.cycle_end(open))
    }

    fn cycle_end(&self, open: &OpenCycle) -> Action {
        let (full, missing) = schedule::coverage(&open.steps);
        let cause = distinct(std::iter::once(open.start).chain(self.checking.last_seq()));
        Action::Decide(DecisionAction::new(
            CheckingCycle {
                cycle: open.number,
                event: CycleEvent::End,
                passed: true,
                full,
                missing,
                steps: Vec::new(),
                reason: self.cycle_end_reason(full),
            },
            cause,
        ))
    }

    fn cycle_end_reason(&self, full: bool) -> Option<String> {
        if !full {
            return None;
        }
        if self.phases.phase1_end().is_none() && !self.any_searching() {
            let moved = self
                .phase2_moves(&self.offsets(), None)
                .map_or_else(Vec::new, |moves| moves.moved);
            let text = if moved.is_empty() {
                "phase 1 ends: first confirmed profile; no core can move, phase 2's confirmation cycle follows".to_owned()
            } else {
                let names: Vec<String> = moved
                    .iter()
                    .map(|core| format!("{:02}", core.get()))
                    .collect();
                format!(
                    "phase 1 ends: first confirmed profile; movable cores {}",
                    names.join(", ")
                )
            };
            return Some(text);
        }
        (self.phases.confirm_start().is_some() && self.phases.concluded().is_none()).then(|| {
            "phase 2 ends: the confirmation cycle passed, so this is the confirmed profile"
                .to_owned()
        })
    }

    /// Starts the next cycle with the configured schedule.
    pub(crate) fn cycle_start_action(&self) -> Action {
        let next = self
            .checking
            .last_cycle()
            .map_or(1, |last| last.get().saturating_add(1));
        let confirming = self.phases.phase1_end().is_some() && self.phases.concluded().is_none();
        Action::Decide(DecisionAction::new(
            CheckingCycle {
                cycle: NonZeroU32::new(next).unwrap_or(NonZeroU32::MIN),
                event: CycleEvent::Start,
                passed: false,
                full: false,
                missing: Vec::new(),
                steps: self.config.cycle.to_vec(),
                reason: confirming.then(|| "phase 2 confirmation cycle".to_owned()),
            },
            self.checking.last_seq().into_iter().collect(),
        ))
    }

    /// A cycle or round decision cites the carried facts that answered reruns since the last one.
    pub(crate) fn after_reruns(&self, action: Action) -> Action {
        let citations = self.reruns.citations();
        if citations.is_empty() {
            return action;
        }
        let Action::Decide(mut decision) = action else {
            return action;
        };
        let text = format!(
            "rerun checks passed{}",
            crate::ledger::carried_clause(&self.evidence, citations)
        );
        let reason = match &mut decision.payload {
            DecisionPayload::CheckingCycle(cycle) => &mut cycle.reason,
            DecisionPayload::DeepeningRound(round) => &mut round.reason,
            DecisionPayload::Failure(_)
            | DecisionPayload::TunerWarning(_)
            | DecisionPayload::DeadEnd(_)
            | DecisionPayload::CorePhase(_)
            | DecisionPayload::TunerDecision(_)
            | DecisionPayload::ProfileChange(_)
            | DecisionPayload::CheckingStep(_)
            | DecisionPayload::CheckingChain(_)
            | DecisionPayload::HuntStart(_)
            | DecisionPayload::HuntGroup(_)
            | DecisionPayload::HuntEnd(_)
            | DecisionPayload::HuntSkipped(_)
            | DecisionPayload::Combination(_) => return Action::Decide(decision),
        };
        *reason = joined(reason.take().into_iter().chain(std::iter::once(text)));
        decision.cause = distinct(decision.cause.iter().chain(citations).copied());
        Action::Decide(decision)
    }

    /// Closes the open cycle for a reset core.
    pub(crate) fn cycle_reset_end(
        &self,
        core: togi_machine::CoreId,
        queued: Seq,
    ) -> Option<DecisionAction> {
        let open = self.checking.open()?;
        Some(DecisionAction::new(
            CheckingCycle {
                cycle: open.number,
                event: CycleEvent::End,
                passed: false,
                full: false,
                missing: Vec::new(),
                steps: Vec::new(),
                reason: Some(format!("core {:02} was reset", core.get())),
            },
            vec![queued],
        ))
    }
}
