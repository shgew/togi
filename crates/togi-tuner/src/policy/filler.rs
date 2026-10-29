//! Night filler target selection and durable rotation.
use super::{
    State,
    mapping::MappingPlan,
    night_next::{Chosen, rotated},
    night_query::NotReady,
};
use crate::{
    UnitId,
    cap::EDGE_MARGIN,
    night::{EdgeU, Layout, Reuse, mapping_cells, qualification_cells},
};
use std::collections::BTreeMap;
use togi_journal::{Activity, NightId, Queue as JournalQueue, Seq, TaskId, Tested};
use togi_machine::{CcdId, TargetOffset};
impl State {
    /// The filler, run while no queue has due work. A cap probe first: on the CCD whose S is furthest from
    /// its observed or censored edge U (largest S − U, ties to the lowest CCD), the mapping matrix one count
    /// deeper than S, where that offset is admissible (at least U + `EDGE_MARGIN`). Otherwise D's exact
    /// qualification cells. The durable filler cursor, the count of filler trials before this one across
    /// every night, rotates the matrix and picks its cell round-robin, skipping cells that need an excluded
    /// backend. Neither form finishes a mapping step. `None` when every such cell needs an excluded backend.
    pub(super) fn filler(
        &self,
        layout: &Layout<'_>,
        mapping: &MappingPlan,
        night: NightId,
        unavailable: &BTreeMap<togi_machine::Backend, Vec<Seq>>,
    ) -> Result<Option<Chosen>, NotReady> {
        let task = TaskId {
            queue: JournalQueue::Filler,
            ordinal: self.night_ordinal(night),
        };
        let cursor = self.trials.filler_cursor();
        let caps = self.permission()?.limits;
        let mut probe: Option<(CcdId, TargetOffset, i64)> = None;
        for ccd in self.ccds() {
            let Some(edge) = mapping.edges.get(&ccd).and_then(EdgeU::offset) else {
                continue;
            };
            let Some(cap) = caps.get(UnitId::Ccd(ccd)) else {
                continue;
            };
            let Ok(deeper) = TargetOffset::new(cap.get() - 1) else {
                continue;
            };
            if deeper.get() < edge.get() + i64::from(EDGE_MARGIN) {
                continue;
            }
            let distance = cap.get() - edge.get();
            if probe.is_none_or(|(_, _, furthest)| distance > furthest) {
                probe = Some((ccd, deeper, distance));
            }
        }
        if let Some((ccd, offset, _)) = probe {
            let matrix = mapping_cells(layout, UnitId::Ccd(ccd), offset, cursor)?;
            if let Some(cell) = rotated(&matrix, cursor, unavailable) {
                return Ok(Some(Chosen {
                    task,
                    activity: Activity::Mapping,
                    cell,
                    profile: matrix.profile,
                    tested: Tested::Isolated { ccd, offset },
                    retry: false,
                }));
            }
        }
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        let matrix =
            qualification_cells(layout, &history.current().profile, cursor, &Reuse::AllFresh)?;
        Ok(rotated(&matrix, cursor, unavailable).map(|cell| Chosen {
            task,
            activity: Activity::Qualification,
            cell,
            profile: matrix.profile,
            tested: Tested::Exact {},
            retry: false,
        }))
    }
}
