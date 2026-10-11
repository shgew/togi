use togi_journal::{CorePhase, CoreTransition, DeadEnd, DeadEndCondition, ProfileChange, Seq};
use togi_machine::TargetOffset;

use crate::state::Run;
use crate::{Action, CombinationId, DecisionAction, Profile};

impl Run {
    pub(crate) fn offsets(&self) -> Profile {
        Profile::over(self.topo.all(), |core| self.core(core).offset)
    }

    /// Every core has had its first phase; before that nothing can be decided.
    fn ready(&self) -> bool {
        self.topo
            .order()
            .iter()
            .all(|&core| self.core(core).phase.is_some())
    }

    /// The pending monotonicity warning, citing the failure and the carried passes it contradicts.
    pub(crate) fn warning_decision(&self) -> Option<DecisionAction> {
        let pending = self.warning.as_ref()?;
        Some(DecisionAction::new(
            pending.warning.clone(),
            pending.cause.clone(),
        ))
    }

    /// A reset queued by `command.reset`: the core returns to search at its baseline, shallowest
    /// first of the clamp to [-50, 0], its failure point, pass and combinations cleared.
    pub(crate) fn queued_reset(&self) -> Option<DecisionAction> {
        let (id, core, queued) = self.topo.order().iter().find_map(|&id| {
            let core = self.core(id);
            core.queued_reset.map(|queued| (id, core, queued))
        })?;
        let cleared: Vec<CombinationId> = self
            .constraints
            .combinations_with(id)
            .map(crate::Combination::id)
            .collect();
        let labels: Vec<String> = cleared.iter().map(ToString::to_string).collect();
        let what = if labels.is_empty() {
            "failure point".to_owned()
        } else {
            format!("failure point and combinations {}", labels.join(", "))
        };
        Some(DecisionAction::new(
            CoreTransition {
                core: id,
                from: core.phase,
                to: CorePhase::Search,
                offset: TargetOffset::clamped(core.baseline.0),
                pass: None,
                failure_point: None,
                check_solo_limit: false,
                workloads: Vec::new(),
                cleared_combination: cleared.iter().map(|id| id.non_zero()).collect(),
                reason: format!(
                    "reset: {what} cleared; search restarts from the baseline {}",
                    core.baseline.0
                ),
            },
            vec![queued],
        ))
    }

    /// A failure point at 0 stays until reset: nothing runs while one stands.
    pub(crate) fn zero_point_dead_end(&self) -> Option<DecisionAction> {
        self.topo.order().iter().find_map(|&id| {
            (self.failure_point(id) == Some(TargetOffset::ZERO)).then(|| {
                DecisionAction::new(
                    DeadEnd {
                        condition: DeadEndCondition::FailureAtZero,
                        core: Some(id),
                        detail: format!(
                            "core {:02} has a failure point at CO 0; only reset can clear it",
                            id.get()
                        ),
                        action: None,
                    },
                    self.core(id).zero_seq.into_iter().collect(),
                )
            })
        })
    }

    /// An unattributed failure with every core at 0 has no candidate to hunt.
    pub(crate) fn unattributed_zero_dead_end(&self) -> Option<DecisionAction> {
        let queued = self.failures.queued().filter(|record| record.profile.is_zero())?;
        Some(DecisionAction::new(
            DeadEnd {
                condition: DeadEndCondition::FailureAtZero,
                core: None,
                detail: "unattributed failure with every core at CO 0; the instability is not caused by Curve Optimizer".to_owned(),
                action: None,
            },
            vec![queued.seq],
        ))
    }

    /// What a search core's attributed failure decides: it backs off or checks its solo limit.
    pub(crate) fn pending_search_decision(&self) -> Option<DecisionAction> {
        self.topo.order().iter().find_map(|&id| {
            let core = self.core(id);
            let seq = core.pending?;
            (core.phase == Some(CorePhase::Search))
                .then(|| DecisionAction::new(self.search_failure(id), vec![seq]))
        })
    }

    /// The first non-search core whose has-room or at-limit phase disagrees with the profile.
    pub(crate) fn phase_decision(&self) -> Option<DecisionAction> {
        let profile = self.offsets();
        self.topo.order().iter().find_map(|&id| {
            let core = self.core(id);
            let phase = core.phase.filter(|&phase| phase != CorePhase::Search)?;
            let limit = self.constraints.at_limit(id, &profile);
            let want = if limit.is_some() {
                CorePhase::AtLimit
            } else {
                CorePhase::HasRoom
            };
            (phase != want).then(|| {
                DecisionAction::new(
                    CoreTransition {
                        core: id,
                        from: Some(phase),
                        to: want,
                        offset: core.offset,
                        pass: core.pass,
                        failure_point: self.failure_point(id),
                        check_solo_limit: false,
                        workloads: Vec::new(),
                        cleared_combination: Vec::new(),
                        reason: limit.map_or_else(
                            || "one count deeper reaches no failure point or combination".to_owned(),
                            |limit| limit.to_string(),
                        ),
                    },
                    core.last_decision.into_iter().collect(),
                )
            })
        })
    }

    /// Once no core searches, the checking profile follows the decided offsets.
    pub(crate) fn profile_decision(&self) -> Option<DecisionAction> {
        if self.any_searching() {
            return None;
        }
        let offsets = self.offsets();
        let from = self.checking.profile();
        if from == Some(offsets) {
            return None;
        }
        let since = self.checking.seq();
        let mut cause: Vec<Seq> = self
            .topo
            .order()
            .iter()
            .filter_map(|&id| self.core(id).last_decision)
            .filter(|&seq| Some(seq) > since)
            .collect();
        cause.sort_unstable();
        Some(DecisionAction::new(
            ProfileChange {
                from: from.map(|profile| profile.offsets()),
                to: offsets.offsets(),
            },
            cause,
        ))
    }

    /// The next thing the tuner does, from the rules the folded state decides alone.
    pub(crate) fn decide(&self) -> Option<Action> {
        if !self.ready() {
            return None;
        }
        let decision = self
            .attribution()
            .or_else(|| self.warning_decision())
            .or_else(|| self.queued_reset())
            .or_else(|| self.zero_point_dead_end())
            .or_else(|| self.unattributed_zero_dead_end())
            .or_else(|| self.pending_search_decision())
            .or_else(|| self.phase_decision())
            .or_else(|| self.profile_decision());
        let action = match decision {
            Some(decision) => Action::Decide(decision),
            None => self.search_action()?,
        };
        Some(self.skip_known_failure(action))
    }
}
