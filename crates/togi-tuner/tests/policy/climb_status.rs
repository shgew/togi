//! The status view of an unfinished unit shows what blocks the next day decision now, not what the last
//! committed hold recorded.

use togi_journal::{ClimbAction, ClimbBlocker};
use togi_tuner::policy::{ClimbUnitState, Projection};

use crate::{
    fixture::{Ending, Journal},
    flow::{cold_boot, steps},
};

/// The blockers every unfinished unit of `projection` shows, one list per unit.
fn held_reasons(projection: &Projection) -> Vec<Vec<ClimbBlocker>> {
    projection
        .climb
        .units
        .iter()
        .map(|unit| match &unit.state {
            ClimbUnitState::Held { reason } => reason.clone(),
            ClimbUnitState::Climbing | ClimbUnitState::Finished { .. } => {
                panic!("expected a held unit, got {:?}", unit.state)
            }
        })
        .collect()
}

#[test]
fn a_night_that_maps_the_edges_after_a_hold_replaces_its_unknown_edge_blockers() {
    let mut journal = Journal::ready();
    journal.boot_start("day1");
    let first = journal.settle_day(&cold_boot("day1", 100));
    let recorded = steps(&first)
        .into_iter()
        .find_map(|step| match &step.decision {
            ClimbAction::Hold { blocked_by } => Some(blocked_by.clone()),
            _ => None,
        })
        .expect("the first cold boot holds");
    assert!(
        recorded
            .iter()
            .any(|blocker| matches!(blocker, ClimbBlocker::UnknownEdge { .. })),
        "{recorded:?}"
    );
    for reason in held_reasons(&journal.state.project()) {
        assert_eq!(reason, recorded);
    }

    // The night maps both CCDs' edges; no new decision has been committed yet.
    journal.reboot("night1");
    journal.until_filler(|_| Ending::Pass);
    let projection = journal.state.project();
    let shown = held_reasons(&projection);
    for reason in &shown {
        assert!(
            !reason
                .iter()
                .any(|blocker| matches!(blocker, ClimbBlocker::UnknownEdge { .. })),
            "stale unknown edge: {reason:?}"
        );
    }

    // What status shows is what the next cold boot's decision records.
    journal.boot_start("day2");
    let next = journal.settle_day(&cold_boot("day2", 60_000));
    let recorded = steps(&next)
        .into_iter()
        .find_map(|step| match &step.decision {
            ClimbAction::Hold { blocked_by } => Some(blocked_by.clone()),
            _ => None,
        })
        .expect("the second cold boot holds");
    for reason in shown {
        assert_eq!(reason, recorded);
    }
}
