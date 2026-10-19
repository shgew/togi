use std::collections::HashMap;

use togi_journal::{Activity, Outcome, Seq, TrialIntent};
use togi_machine::{Condition, CoreId, CoreSet, PerCore};

use super::respond::Failed;
use crate::flight::OpenTrial;
use crate::state::Run;
use crate::{
    Action, ClassKey, Load, LoadKey, Requirement, TrialContext, TrialPlan, Window, WorkloadChoice,
};

/// One unfinished experiment, not a claim that the idled group caused the parent failure.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct Probe {
    class: ClassKey,
    since: Seq,
    failure: Seq,
    decisions: Vec<Seq>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct Charge {
    previous: Option<Seq>,
    probe: Option<Probe>,
}

/// At most one previous charge and one pending probe per (load, charged core). Parent passes
/// deliberately do not clear this: an intermittent failure can ratchet a top requester too.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Probes {
    #[serde(with = "togi_machine::serde_pairs")]
    charges: HashMap<LoadKey, PerCore<Option<Charge>>>,
}

impl Probes {
    pub(crate) fn due(&self, load: LoadKey, core: CoreId, stalled_elsewhere: bool) -> bool {
        stalled_elsewhere
            || self
                .charges
                .get(&load)
                .and_then(|cores| cores.get_ref(core))
                .and_then(Option::as_ref)
                .is_some_and(|charge| charge.previous.is_some())
    }

    pub(crate) fn charge(
        &mut self,
        key: (LoadKey, CoreId),
        class: ClassKey,
        decision: Seq,
        failure: Seq,
        stalled_elsewhere: bool,
    ) {
        let charge = self
            .charges
            .entry(key.0)
            .or_default()
            .get_or_insert_with(key.1, Charge::default);
        if charge.probe.is_some() {
            return;
        }
        if charge.previous.is_some() || stalled_elsewhere {
            let mut decisions: Vec<Seq> = charge.previous.into_iter().collect();
            decisions.push(decision);
            charge.previous = None;
            charge.probe = Some(Probe {
                class,
                since: decision,
                failure,
                decisions,
            });
        } else {
            charge.previous = Some(decision);
        }
    }

    fn head(&self) -> Option<&Probe> {
        self.charges
            .values()
            .flat_map(|cores| cores.iter_ref().filter_map(|(_, charge)| charge.as_ref()))
            .filter_map(|charge| charge.probe.as_ref())
            .min_by_key(|probe| probe.since)
    }

    pub(crate) fn reset_core(&mut self, core: CoreId) {
        self.charges.retain(|load, _| !load.loaded.contains(core));
    }
}

impl Run {
    /// Idle the whole tied group containing the charged core, on just its CCD. An all-core
    /// experiment keeps every other CCD loaded; a singleton is never a partial R7 experiment.
    pub(crate) fn r7_probe_part(
        &self,
        view: &Failed<'_>,
        core: CoreId,
    ) -> Option<(ClassKey, CoreSet, bool)> {
        let record = view.record;
        if record.pending.carried
            || record.pending.fact.failure.condition != Some(Condition::Together)
            || self.named_culprit(view)
        {
            return None;
        }
        let ccd = self.topo.ccd_of(core)?;
        let idled = Self::ccd_groups(view, ccd)
            .into_iter()
            .find(|group| group.contains(core))?;
        let loaded = record.pending.fact.class.loaded();
        let rest: CoreSet = loaded.iter().filter(|&id| !idled.contains(id)).collect();
        if rest.len() < 2 {
            return None;
        }
        let stalled_elsewhere = view.stalled.is_some_and(|stalled| {
            rest.contains(stalled) && self.topo.ccd_of(stalled) == Some(ccd)
        });
        Some((
            ClassKey::new(
                record.pending.fact.class.workload(),
                rest,
                record.pending.fact.class.duration(),
            ),
            idled,
            stalled_elsewhere,
        ))
    }

    fn r7_probe_requirement(&self, probe: &Probe) -> Requirement {
        Requirement {
            context: TrialContext::Rerun {
                failure: probe.failure,
            },
            class: probe.class,
            window: Window::Live {
                since: Some(probe.since),
            },
            needed: self.config.needed,
        }
    }

    /// An ordinary together rerun, identified by its exact partial class and charge sequence.
    /// No journal field or new trial condition is needed to reconstruct the experiment.
    pub(crate) fn r7_probe_action(&self) -> Option<Action> {
        let probe = self.r7.probes.head()?;
        let profile = self.checking.profile()?;
        let requirement = self.r7_probe_requirement(probe);
        let mut cause = vec![probe.failure];
        cause.extend(probe.decisions.iter().copied());
        if let Some(plan) = self.serve_checking_retry(&requirement, cause.clone()) {
            return Some(Action::RunTrial(plan));
        }
        let class = probe.class;
        Some(Action::RunTrial(TrialPlan {
            regime: class.regime(),
            workload: WorkloadChoice::Frozen(class.workload()),
            load: Load::Many(class.loaded()),
            condition: Condition::Together,
            activity: Activity::Checking,
            profile,
            duration: class.duration(),
            requirement,
            retry: false,
            cause,
        }))
    }

    pub(crate) fn r7_probe_context(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
    ) -> Option<Requirement> {
        let probe = self.r7.probes.head()?;
        (intent.rerun
            && intent.cycle.is_none()
            && intent.step.is_none()
            && class == probe.class
            && Self::together_shape(intent, class))
        .then(|| self.r7_probe_requirement(probe))
    }

    pub(crate) fn r7_probe_retry_requirement(
        &self,
        plan: &TrialPlan,
        failure: Seq,
    ) -> Option<Requirement> {
        let probe = self
            .r7
            .probes
            .head()
            .filter(|probe| probe.failure == failure)?;
        let WorkloadChoice::Frozen(workload) = plan.workload else {
            return None;
        };
        (ClassKey::new(workload, plan.load.cores(), plan.duration) == probe.class)
            .then(|| self.r7_probe_requirement(probe))
    }

    /// A first failure ends the experiment and is ordinary R7 failure evidence. Passing needs
    /// n fresh valid trials at the intended failed duration, not a shortened crash duration.
    pub(crate) fn r7_probe_end(&mut self, open: &OpenTrial, outcome: Outcome) {
        let Some(probe) = self.r7.probes.head() else {
            return;
        };
        if open.requirement != self.r7_probe_requirement(probe) {
            return;
        }
        let done = match outcome {
            Outcome::Failure => true,
            Outcome::Pass => self
                .evidence
                .count(
                    probe.class,
                    &open.profile,
                    open.requirement.window,
                    self.config.needed,
                )
                .is_ok_and(|count| count.valid >= u32::from(self.config.needed.get())),
            Outcome::Inconclusive => false,
        };
        if done {
            let since = probe.since;
            self.r7.probes.charges.retain(|_, cores| {
                for (_, charge) in cores.iter_mut() {
                    if charge
                        .as_ref()
                        .and_then(|charge| charge.probe.as_ref())
                        .is_some_and(|probe| probe.since == since)
                    {
                        *charge = None;
                    }
                }
                cores.iter_ref().any(|(_, charge)| charge.is_some())
            });
        }
    }
}
