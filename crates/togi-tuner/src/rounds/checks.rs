use togi_journal::Seq;
use togi_machine::{Condition, CoreSet, Regime, WorkloadId, pick_workload, workloads};

use super::{Check, Round};
use crate::r7::order::RequestOrder;
use crate::state::Run;
use crate::{ClassKey, LoadKey, Profile};

impl Run {
    /// The checks a round needs and the measurements that ordered the R7 ones. Each deepened
    /// core runs R1 then R2 alone at its proposed offset; each R7 workload runs the part of
    /// every CCD whose top request group holds a deepened core, and the CCD whole when a core
    /// that topped it at the baseline deepened.
    pub(crate) fn round_checks(&self, round: &Round) -> (Vec<Check>, Vec<Seq>) {
        let shape = round.shape();
        let mut checks = Vec::new();
        for &core in self.topo.order() {
            if !shape.deepened(core) {
                continue;
            }
            let Some(offset) = shape.profile().get(core) else {
                continue;
            };
            let (origin_round, since) = self
                .phases
                .mark(core)
                .map_or((shape.id(), shape.start()), |mark| (mark.round, mark.seq));
            let rotation = crate::numbers::index(origin_round.get() - 1);
            for regime in [Regime::R1, Regime::R2] {
                checks.push(Check {
                    class: ClassKey::new(
                        pick_workload(regime, rotation).id(),
                        std::iter::once(core).collect(),
                        round.trial_s(),
                    ),
                    condition: Condition::Alone,
                    profile: Profile::alone(&self.topo, core, offset),
                    since,
                });
            }
        }
        let since = shape
            .cores()
            .iter()
            .filter(|&core| shape.deepened(core))
            .filter_map(|core| self.phases.mark(core).map(|mark| mark.seq))
            .min()
            .unwrap_or(shape.start());
        let mut sources = Vec::new();
        for workload in workloads(Regime::R7) {
            for &(_, ccd) in self.topo.ccds() {
                self.r7_checks(round, workload.id(), ccd, since, &mut checks, &mut sources);
            }
        }
        sources.sort_unstable();
        sources.dedup();
        (checks, sources)
    }

    fn r7_checks(
        &self,
        round: &Round,
        workload: WorkloadId,
        full: CoreSet,
        since: Seq,
        checks: &mut Vec<Check>,
        sources: &mut Vec<Seq>,
    ) {
        let shape = round.shape();
        let order_of = |load: CoreSet, profile: &Profile| {
            RequestOrder::derive(
                &self.r7.measurements,
                &self.topo,
                LoadKey::new(workload, load),
                profile,
            )
        };
        let initial = order_of(full, shape.baseline());
        sources.extend_from_slice(initial.sources());
        let initial_deepened = initial.top().iter().any(|core| shape.deepened(core));
        let mut previous = full;
        while !previous.is_empty() {
            let order = order_of(previous, shape.profile());
            sources.extend_from_slice(order.sources());
            let top = order.top();
            if top.is_empty() {
                break;
            }
            let deepened = top.iter().any(|core| shape.deepened(core));
            if deepened || (previous == full && initial_deepened) {
                let class = ClassKey::new(workload, previous, round.trial_s());
                if !checks.iter().any(|check| check.class == class) {
                    checks.push(Check {
                        class,
                        condition: Condition::Together,
                        profile: *shape.profile(),
                        since,
                    });
                }
            }
            previous = previous
                .iter()
                .filter(|&core| !top.contains(core))
                .collect();
            if previous.len() < 2 {
                break;
            }
        }
    }
}
