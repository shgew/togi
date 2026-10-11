use std::collections::BTreeMap;

use togi_journal::{
    Activity, CorePhase, CoreTransition, DeadEnd, DeadEndCondition, Decision, Seq, TunerDecision,
};
use togi_machine::{Backend, Condition, CoreId, Regime, TargetOffset, WorkloadId, pick_workload};

use crate::cores::Core;
use crate::ledger::carried_clause;
use crate::state::Run;
use crate::{
    Action, BackendId, ClassKey, DecisionAction, DecisionPayload, Depth, Evidence, Load, Profile,
    TrialContext, TrialPlan, Window, WorkloadChoice,
};

/// How many counts shallower than its solo limit a core starts checking.
const MARGIN: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StepPass {
    seq: Seq,
    backend: Backend,
}

type StepEvidence = BTreeMap<BackendId, StepPass>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Step {
    r1: StepEvidence,
    r2: StepEvidence,
}

/// Where the search turn stands: the core that ran last, and for each core the first R1 and R2
/// pass of its unfinished step under each backend package.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Search {
    cursor: Option<usize>,
    steps: BTreeMap<CoreId, Step>,
}

impl Search {
    pub(crate) fn turn(&mut self, cursor: Option<usize>) {
        self.cursor = cursor;
    }

    pub(crate) const fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub(crate) fn clear(&mut self, core: CoreId) {
        self.steps.remove(&core);
    }

    pub(crate) fn record_pass(
        &mut self,
        core: CoreId,
        r1: bool,
        backend: Backend,
        id: BackendId,
        seq: Seq,
    ) {
        let step = self.steps.entry(core).or_default();
        let passes = if r1 { &mut step.r1 } else { &mut step.r2 };
        passes.entry(id).or_insert(StepPass { seq, backend });
    }

    fn first_valid(passes: &StepEvidence, evidence: &Evidence) -> Option<Seq> {
        passes
            .iter()
            .filter(|&(&id, pass)| evidence.active_backend(pass.backend) == id)
            .map(|(_, pass)| pass.seq)
            .min()
    }

    /// The seqs of the core's first R1 and R2 passes that ran under the backend packages now in
    /// force.
    pub(crate) fn current(&self, core: CoreId, evidence: &Evidence) -> (Option<Seq>, Option<Seq>) {
        self.steps.get(&core).map_or((None, None), |step| {
            (
                Self::first_valid(&step.r1, evidence),
                Self::first_valid(&step.r2, evidence),
            )
        })
    }
}

#[derive(Clone, Copy)]
struct View {
    core: CoreId,
    offset: TargetOffset,
    pass: Option<TargetOffset>,
    fail: Option<TargetOffset>,
    checks: u32,
}

impl Run {
    fn view(&self, id: CoreId) -> View {
        let core = self.core(id);
        View {
            core: id,
            offset: core.offset,
            pass: core.pass,
            fail: self.failure_point(id),
            checks: core.checks,
        }
    }

    fn check_pair(view: View) -> [WorkloadId; 2] {
        let k = usize::try_from(view.checks).unwrap_or(usize::MAX);
        [
            pick_workload(Regime::R1, k).id(),
            pick_workload(Regime::R2, k).id(),
        ]
    }

    fn check_solo_limit(
        &self,
        view: View,
        offset: TargetOffset,
        fail: Option<TargetOffset>,
        label: &str,
    ) -> DecisionPayload {
        let [r1, r2] = Self::check_pair(view);
        TunerDecision {
            core: view.core,
            activity: Activity::Search,
            decision: Decision::CheckSoloLimit,
            from_offset: view.offset,
            to_offset: offset,
            pass: Some(offset),
            failure_point: fail,
            workloads: vec![r1, r2],
            reason: format!(
                "candidate solo limit {}: {label}; the full pass rule needs {} trials each of R1 {r1} and R2 {r2}",
                offset.get(),
                self.config.needed
            ),
        }
        .into()
    }

    fn search_pass(&self, view: View) -> DecisionPayload {
        let offset = view.offset;
        let pass = Some(offset);
        if offset.at_floor() {
            return self.check_solo_limit(view, offset, view.fail, "floor");
        }
        let Some(fail) = view.fail else {
            return TunerDecision {
                core: view.core,
                activity: Activity::Search,
                decision: Decision::StepDeeper,
                from_offset: offset,
                to_offset: offset.deeper_or_floor(5),
                pass,
                failure_point: None,
                workloads: Vec::new(),
                reason: "coarse, no failure point yet".to_owned(),
            }
            .into();
        };
        let next = offset.deeper_by(1);
        if next == Some(fail) {
            return self.check_solo_limit(
                view,
                offset,
                Some(fail),
                &format!("failure point {}", fail.get()),
            );
        }
        TunerDecision {
            core: view.core,
            activity: Activity::Search,
            decision: Decision::StepDeeper,
            from_offset: offset,
            to_offset: next.unwrap_or(offset),
            pass,
            failure_point: Some(fail),
            workloads: Vec::new(),
            reason: format!("fine, failure point {}", fail.get()),
        }
        .into()
    }

    /// What a search core's attributed failure decides. `failed` is the offset the failure
    /// recorded, which a skipped known failure takes from the failure it repeats; the move starts
    /// from the core's current offset.
    pub(crate) fn search_failure(&self, id: CoreId, failed: TargetOffset) -> DecisionPayload {
        let view = self.view(id);
        let offset = failed;
        if offset == TargetOffset::ZERO {
            return DeadEnd {
                condition: DeadEndCondition::FailureAtZero,
                core: Some(id),
                detail: format!(
                    "core {:02} failed at CO 0; the instability is not caused by Curve Optimizer",
                    id.get()
                ),
                action: None,
            }
            .into();
        }
        let fail = view.fail.map_or(offset, |old| {
            if old.shallower_than(offset) {
                old
            } else {
                offset
            }
        });
        let (pass, discarded) = match view.pass {
            Some(pass) if !pass.shallower_than(fail) => (
                None,
                format!(
                    "; passed step at {} discarded, the failure contradicts it",
                    pass.get()
                ),
            ),
            kept => (kept, String::new()),
        };
        let Some(pass) = pass else {
            return TunerDecision {
                core: id,
                activity: Activity::Search,
                decision: Decision::Backoff,
                from_offset: view.offset,
                to_offset: offset.shallower_by(5),
                pass: None,
                failure_point: Some(fail),
                workloads: Vec::new(),
                reason: format!("coarse, no passed step{discarded}"),
            }
            .into();
        };
        let below = pass.deeper_by(1);
        if below == Some(fail) {
            return self.check_solo_limit(
                view,
                pass,
                Some(fail),
                &format!("failure point {}{discarded}", fail.get()),
            );
        }
        TunerDecision {
            core: id,
            activity: Activity::Search,
            decision: Decision::Backoff,
            from_offset: view.offset,
            to_offset: below.unwrap_or(pass),
            pass: Some(pass),
            failure_point: Some(fail),
            workloads: Vec::new(),
            reason: format!("fine, deepest pass {}{discarded}", pass.get()),
        }
        .into()
    }

    fn decide_core(payload: DecisionPayload, cause: Vec<Seq>) -> Action {
        Action::Decide(DecisionAction { payload, cause })
    }

    pub(crate) fn alone_class(&self, id: CoreId, workload: WorkloadId) -> ClassKey {
        ClassKey::new(workload, Load::Single(id).cores(), self.config.search_trial)
    }

    pub(crate) fn rotation_workload(&self, id: CoreId, regime: Regime) -> WorkloadId {
        pick_workload(regime, self.core(id).rotation.get(regime)).id()
    }

    pub(crate) fn search_trial(
        &self,
        id: CoreId,
        regime: Regime,
        frozen: Option<WorkloadId>,
    ) -> TrialPlan {
        let core = self.core(id);
        let workload = frozen.unwrap_or_else(|| self.rotation_workload(id, regime));
        let context = if core.check.is_some() {
            TrialContext::SoloLimit { core: id, regime }
        } else {
            TrialContext::Search {
                core: id,
                regime,
                offset: core.offset,
            }
        };
        TrialPlan {
            regime,
            workload: frozen.map_or(WorkloadChoice::Rotate, WorkloadChoice::Frozen),
            load: Load::Single(id),
            condition: Condition::Alone,
            activity: Activity::Search,
            profile: Profile::alone(&self.topo, id, core.offset),
            duration: self.config.search_trial,
            context,
            requirement: self.solo_requirement(id, self.alone_class(id, workload)),
            retry: false,
            cause: core.last_seq.into_iter().collect(),
        }
    }

    pub(crate) fn searching(&self, id: CoreId) -> Option<Core> {
        Some(self.core(id)).filter(|core| core.phase == Some(CorePhase::Search))
    }

    fn candidate_exit(&self, id: CoreId, core: Core) -> Option<Action> {
        let check = core.check?;
        let phase_seq = core.phase_seq;
        let profile = Profile::alone(&self.topo, id, core.offset);
        let mut cause: Vec<Seq> = phase_seq.into_iter().collect();
        for (regime, frozen) in [(Regime::R1, check.r1), (Regime::R2, check.r2)] {
            let class = self.alone_class(id, frozen);
            let count = self
                .evidence
                .count(
                    class,
                    &profile,
                    Window::LiveOrCarried { since: phase_seq },
                    self.config.needed,
                )
                .unwrap_or_default();
            if count.valid < u32::from(self.config.needed.get()) {
                return Some(Action::RunTrial(self.search_trial(
                    id,
                    regime,
                    Some(frozen),
                )));
            }
            for seq in count.witnesses {
                if self.evidence.carried_source(seq).is_some() && !cause.contains(&seq) {
                    cause.push(seq);
                }
            }
        }
        let start = core.offset.shallower_by(MARGIN);
        let at = Profile::over(self.topo.all(), |other| {
            if other == id {
                start
            } else {
                self.core(other).offset
            }
        });
        let limit = self.constraints.at_limit(id, &at).ok().flatten();
        let (phase, reason) = match limit {
            Some(limit) => (CorePhase::AtLimit, limit.to_string()),
            None => (
                CorePhase::HasRoom,
                "one count deeper reaches no failure point or combination".to_owned(),
            ),
        };
        let solo = core.offset.get();
        let reason = format!(
            "solo limit {solo} passed {} trials of R1 {} and R2 {}{}; checking starts at {}, one count shallower than the solo limit {solo} (margin); {reason}",
            self.config.needed,
            check.r1,
            check.r2,
            carried_clause(&self.evidence, &cause),
            start.get(),
        );
        Some(Self::decide_core(
            CoreTransition {
                core: id,
                from: Some(CorePhase::Search),
                to: phase,
                offset: start,
                pass: Some(core.offset),
                failure_point: self.failure_point(id),
                check_solo_limit: false,
                workloads: Vec::new(),
                cleared_combination: Vec::new(),
                reason,
            }
            .into(),
            cause,
        ))
    }

    /// The search turn: finish the current step, serve a saved alone retry, run the current
    /// turn's R2, else give the next core still in search its R1 or its candidate check.
    pub(crate) fn search_action(&self) -> Option<Action> {
        let order = self.topo.order();
        let cursor = self.search.cursor.and_then(|at| order.get(at).copied());
        if let Some(id) = cursor {
            let current = self.searching(id).filter(|core| core.check.is_none());
            if let (Some(_), (Some(r1), Some(r2))) =
                (current, self.search.current(id, &self.evidence))
            {
                return Some(Self::decide_core(
                    self.search_pass(self.view(id)),
                    vec![r1, r2],
                ));
            }
        }
        if let Some(plan) = self.serve_retry() {
            return Some(Action::RunTrial(plan));
        }
        if let Some(id) = cursor {
            let current = self.searching(id).filter(|core| core.check.is_none());
            if current.is_some()
                && matches!(self.search.current(id, &self.evidence), (Some(_), None))
            {
                return Some(Action::RunTrial(self.search_trial(id, Regime::R2, None)));
            }
        }
        let start = self.search.cursor.map_or(0, |at| at + 1);
        for step in 0..order.len() {
            let id = order[(start + step) % order.len()];
            let Some(core) = self.searching(id) else {
                continue;
            };
            if core.check.is_none() {
                return Some(Action::RunTrial(self.search_trial(id, Regime::R1, None)));
            }
            return self.candidate_exit(id, core);
        }
        None
    }
}
