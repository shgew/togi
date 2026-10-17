use serde::{Deserialize, Serialize};
use togi_journal::Seq;
use togi_machine::{CcdId, CoreId, CoreSet, PerCore, Regime, WorkloadId, workloads};

use super::order::{Lane, RequestOrder};
use crate::state::Run;
use crate::{LoadKey, Profile, State};

/// The request order a chain derivation reads for one load on one CCD at the checking profile
/// (`docs/spec/tuner.md`, R7 request order and attribution).
#[derive(Clone, Debug, PartialEq)]
pub struct ChainOrder {
    /// The loaded cores' request groups, highest first, ties within 1 mV together.
    pub groups: Vec<CoreSet>,
    /// Each loaded core's request in volts, or the offset stand-in of a core no measurement
    /// covers: the values the groups order.
    pub values: PerCore<f64>,
    /// The cores ordered by the offset stand-in.
    pub proxy: CoreSet,
    /// The measurements that supplied a request, ascending.
    pub sources: Vec<Seq>,
}

impl State {
    /// The order the next chain derivation from `load` on `ccd` would read now: from the newest
    /// measurement of exactly that load, else of the CCD's full part, shifted to the checking
    /// profile. `None` before checking has a profile, or when `load` has no core on `ccd`.
    pub fn chain_order(&self, load: LoadKey, ccd: CcdId) -> Option<ChainOrder> {
        let run = self.run().filter(|_| !self.poisoned())?;
        let profile = run.checking.profile()?;
        let order = RequestOrder::derive(&run.r7.measurements, &run.topo, load, &profile);
        let on_ccd = order.ccd(ccd)?;
        let loaded = on_ccd.loaded();
        Some(ChainOrder {
            groups: on_ccd.groups().to_vec(),
            values: loaded
                .iter()
                .filter_map(|core| order.value(core).map(|value| (core, value)))
                .collect(),
            proxy: order
                .proxy()
                .iter()
                .filter(|&core| loaded.contains(core))
                .collect(),
            sources: order.sources().to_vec(),
        })
    }
}

/// Whether a core of an R7 workload has been seen to pass as a top requester at its current
/// offset or deeper. Observed self-sufficiency, not a guarantee against future failures.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct R7CoreStatus {
    pub core: CoreId,
    pub ccd: CcdId,
    pub workload: WorkloadId,
    pub top_requester: bool,
    pub offset_proxy: bool,
    pub self_sufficient: bool,
    pub passes: u32,
}

impl Run {
    /// Per R7 workload, CCD and core in that order, the self-sufficiency under the current
    /// backend identity, with the request order of each CCD's full part at the current offsets.
    pub(crate) fn r7_status(&self) -> Vec<R7CoreStatus> {
        let offsets: Profile = self.offsets();
        let mut out = Vec::new();
        for workload in workloads(Regime::R7) {
            let id = workload.id();
            let backend = self.evidence.active_backend(id.backend());
            for &(ccd, part) in self.topo.ccds() {
                let order = RequestOrder::derive(
                    &self.r7.measurements,
                    &self.topo,
                    LoadKey::new(id, part),
                    &offsets,
                );
                let top = order.top();
                for (core, offset) in offsets.among(part) {
                    let passes = self.r7.status.passes(backend, id, core, offset);
                    out.push(R7CoreStatus {
                        core,
                        ccd,
                        workload: id,
                        top_requester: top.contains(core),
                        offset_proxy: matches!(order.lane(core), Some(Lane::OffsetProxy)),
                        self_sufficient: passes > 0,
                        passes,
                    });
                }
            }
        }
        out
    }
}
