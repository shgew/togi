use togi_journal::{CorePhase, CoreTransition, DeadEnd, DeadEndCondition, ProfileChange, Seq};
use togi_machine::{CoreId, Regime, TargetOffset};

use crate::state::Run;
use crate::{Action, CombinationId, DecisionAction, NotReady, Profile};

fn multi_core_r7(action: &Action) -> bool {
    matches!(action, Action::RunTrial(plan) if plan.regime == Regime::R7 && plan.load.cores().len() > 1)
}

impl Run {
    pub(crate) fn offsets(&self) -> Profile {
        Profile::over(self.topo.all(), |core| self.core(core).offset)
    }

    fn unphased(&self) -> Option<CoreId> {
        self.topo
            .order()
            .iter()
            .copied()
            .find(|&core| self.core(core).phase.is_none())
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
        if let Some(cancel) = self.hunt_cancel(id, queued) {
            return Some(cancel);
        }
        if let Some(end) = self
            .round_reset_end(id, queued)
            .or_else(|| self.cycle_reset_end(id, queued))
        {
            return Some(end);
        }
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
        let queued_seq = self.failures.queue().find(|&seq| {
            self.failures
                .pending(seq)
                .is_some_and(|record| record.profile.is_zero())
        })?;
        Some(DecisionAction::new(
            DeadEnd {
                condition: DeadEndCondition::FailureAtZero,
                core: None,
                detail: "unattributed failure with every core at CO 0; the instability is not caused by Curve Optimizer".to_owned(),
                action: None,
            },
            vec![queued_seq],
        ))
    }

    /// The first non-search core whose has-room or at-limit phase disagrees with the profile.
    pub(crate) fn phase_decision(&self) -> Option<DecisionAction> {
        let profile = self.offsets();
        self.topo.order().iter().find_map(|&id| {
            let core = self.core(id);
            let phase = core.phase.filter(|&phase| phase != CorePhase::Search)?;
            let limit = self.constraints.at_limit(id, &profile).ok().flatten();
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
                            || {
                                "one count deeper reaches no failure point or combination"
                                    .to_owned()
                            },
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

    /// The next thing the tuner does: first what answers a failure or a reset, then the checking
    /// profile and the search, then a queued failure, the reruns owed, and the open cycle or the
    /// next one. A multi-core R7 trial waits until a host ranking has ever been read.
    pub(crate) fn next(&self) -> Result<Action, NotReady> {
        if let Some(core) = self.unphased() {
            return Err(NotReady::NoPhase(core));
        }
        let action = self.next_action().ok_or(NotReady::NoAction)?;
        if self.ranking.is_none() && multi_core_r7(&action) {
            return Ok(Action::ReadRanking);
        }
        Ok(self.skip_known_failure(action))
    }

    /// What the tuner decides before it may stop: attribution, the pending warning, an
    /// unattributed failure at 0, phase-2 blame, the pending failure decision and what closes a
    /// hunt. Never a trial or a ranking read.
    pub(crate) fn drain(&self) -> Option<DecisionAction> {
        if self.unphased().is_some() {
            return None;
        }
        self.attribution()
            .or_else(|| self.warning_decision())
            .or_else(|| self.unattributed_zero_dead_end())
            .or_else(|| self.phase2_blame_action())
            .or_else(|| match self.pending_decision() {
                Some(Action::Decide(decision)) => Some(decision),
                Some(Action::RunTrial(_) | Action::ReadRanking) | None => None,
            })
            .or_else(|| self.hunt_closing())
    }

    fn pending_decision(&self) -> Option<Action> {
        self.ordinary_decision()
            .map(|action| self.later_decision(action))
    }

    /// What answers a failure the ordinary way: the multi-core R7 response, else the attributed
    /// failure of the first core with one pending.
    pub(crate) fn ordinary_decision(&self) -> Option<Action> {
        if let Some(response) = self.r7_decision() {
            return Some(self.round_before_backoff(response));
        }
        self.pending_core_action()
    }

    fn next_action(&self) -> Option<Action> {
        let decision = self
            .attribution()
            .or_else(|| self.warning_decision())
            .or_else(|| self.queued_reset())
            .or_else(|| self.zero_point_dead_end())
            .or_else(|| self.unattributed_zero_dead_end());
        if let Some(decision) = decision {
            return Some(Action::Decide(decision));
        }
        if let Some(decision) = self.phase2_blame_action() {
            return Some(Action::Decide(decision));
        }
        if let Some(action) = self.pending_decision() {
            return Some(action);
        }
        if let Some(action) = self.hunt_next() {
            return Some(action);
        }
        if let Some(decision) = self
            .round_queue_close()
            .or_else(|| self.round_move_action())
        {
            return Some(Action::Decide(decision));
        }
        if let Some(decision) = self.phase_decision().or_else(|| self.profile_decision()) {
            return Some(Action::Decide(decision));
        }
        if self.any_searching() {
            return self.search_action();
        }
        if !self.failures.queue_is_empty() {
            return self.hunt_start_action();
        }
        if let Some(action) = self.locate_action() {
            return Some(action);
        }
        if let Some(action) = self.r7_probe_action() {
            return Some(action);
        }
        if let Some(action) = self.rerun_action() {
            return Some(action);
        }
        if self.rounds.active().is_some() {
            return self
                .round_check_action()
                .map(|action| self.after_reruns(action));
        }
        if self.checking.open().is_some() {
            return self.cycle_action().map(|action| self.after_reruns(action));
        }
        if self.round_due() {
            if self.ranking_stale() {
                return Some(Action::ReadRanking);
            }
            return self
                .round_start_action()
                .map(|action| self.after_reruns(action));
        }
        Some(self.after_reruns(self.cycle_start_action()))
    }
}
