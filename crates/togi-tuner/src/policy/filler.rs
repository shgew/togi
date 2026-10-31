//! Night filler target selection and durable rotation.
use super::{
    State,
    night_next::{Chosen, rotated},
    night_query::NotReady,
};
use crate::night::{Layout, Reuse, qualification_cells};
use std::collections::BTreeMap;
use togi_journal::{Activity, NightId, Queue as JournalQueue, Seq, TaskId, Tested};
impl State {
    /// Repeat the exact current D's qualification cells while no queue has due work. The durable
    /// filler cursor counts trials across every night, rotating the matrix and picking cells
    /// round-robin while skipping excluded backends. Filler never finishes a mapping step.
    /// `None` at stock, or when every cell needs an excluded backend.
    pub(super) fn filler(
        &self,
        layout: &Layout<'_>,
        night: NightId,
        unavailable: &BTreeMap<togi_machine::Backend, Vec<Seq>>,
    ) -> Result<Option<Chosen>, NotReady> {
        let task = TaskId {
            queue: JournalQueue::Filler,
            ordinal: self.night_ordinal(night),
        };
        let cursor = self.trials.filler_cursor();
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        if history.current().profile.is_zero() {
            return Ok(None);
        }
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
