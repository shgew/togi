use togi_journal::{Attribution, Failure};
use togi_machine::{Condition, Regime};

use crate::ledger::{carried_clause, profile_text};
use crate::state::Run;
use crate::{Action, ClassKey, DecisionAction, Load, TrialContext, TrialPlan};

fn cores_text(cores: togi_machine::CoreSet) -> String {
    let names: Vec<String> = cores
        .iter()
        .map(|core| format!("{:02}", core.get()))
        .collect();
    match names.len() {
        1 => format!("core {}", names.join("")),
        _ => format!("cores {}", names.join(", ")),
    }
}

impl Run {
    pub(crate) fn plan_class(&self, plan: &TrialPlan) -> ClassKey {
        ClassKey::new(
            plan.workload_at(self.rotation(plan)),
            plan.load.cores(),
            plan.duration,
        )
    }

    /// Turns a planned trial into the failure it would only repeat: a valid failure at an
    /// equal-or-shallower profile, not yet covered by `n` newer passes, answers it. Multi-core R7
    /// is never skipped; a parked trial is skipped only through a hunt group.
    pub(crate) fn skip_known_failure(&self, action: Action) -> Action {
        let Action::RunTrial(plan) = &action else {
            return action;
        };
        let multi_r7 = plan.regime == Regime::R7 && plan.load.cores().len() > 1;
        let hunt = matches!(plan.requirement.context, TrialContext::Hunt { .. });
        if multi_r7
            || (plan.condition == Condition::Parked && !hunt)
            || matches!(plan.requirement.context, TrialContext::ZeroRerun { .. })
        {
            return action;
        }
        let needed = if hunt {
            self.hunts
                .active
                .as_ref()
                .map_or(self.config.needed, |active| active.needed)
        } else {
            self.config.needed
        };
        let class = self.plan_class(plan);
        let Ok(Some(seq)) = self
            .evidence
            .failing_seq(class, &plan.profile, None, needed)
        else {
            return action;
        };
        let reason = format!(
            "skipped {} {} {} trial on {} for {}s at profile {}: known failure #{seq} at an equal-or-shallower profile is not covered by {needed} newer passes{}",
            plan.condition,
            plan.regime,
            class.workload(),
            cores_text(class.loaded()),
            plan.duration.get(),
            profile_text(&plan.profile),
            carried_clause(&self.evidence, &[seq]),
        );
        if hunt {
            return self
                .skip_hunt_group(plan, seq, reason)
                .map_or(action, Action::Decide);
        }
        if self.later.is_held(seq) {
            return action;
        }
        let Some(known) = self.failures.witnessed(seq) else {
            return action;
        };
        let mut failure: Failure = known.failure.clone();
        failure.known_failure = Some(seq);
        failure.reason = Some(reason);
        failure.round = if let TrialContext::Round { round } = plan.requirement.context {
            Some(round)
        } else {
            None
        };
        failure.condition = Some(plan.condition);
        failure.regime = Some(plan.regime);
        match plan.load {
            Load::Single(core) if plan.condition == Condition::Alone => {
                failure.attribution = Attribution::Attributed;
                failure.core = Some(core);
                failure.offset = known.profile.get(core);
            }
            Load::Single(_) | Load::Many(_) => {
                if failure.attribution == Attribution::Unattributed {
                    if let Some(core) = known.profile.sole_nonzero() {
                        failure.attribution = Attribution::Attributed;
                        failure.core = Some(core);
                        failure.offset = known.profile.get(core);
                    }
                }
            }
        }
        Action::Decide(DecisionAction::new(failure, vec![seq]))
    }
}
