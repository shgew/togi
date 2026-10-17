use serde::{Deserialize, Serialize};
use togi_machine::{CcdId, CoreId, Regime, WorkloadId, workloads};

use super::order::{Lane, RequestOrder};
use crate::state::Run;
use crate::{LoadKey, Profile};

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
