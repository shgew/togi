use std::collections::BTreeMap;

use togi_machine::TargetOffset;
use togi_tuner::night::{
    Cursor, EdgeU, IsolatedObservation, IsolatedResult, Location, MappingBoard, MappingKind,
    StepResult, UnitMapping, write_limit,
};

use crate::fixture::{offset, seq, unit};

fn fail(witness: u64) -> StepResult {
    StepResult::Failed {
        witness: seq(witness),
    }
}

fn charged(witness: u64) -> IsolatedResult {
    IsolatedResult::Fail {
        location: Location::TestedCcd,
        witness: seq(witness),
    }
}

fn target(mapping: &UnitMapping, cap: i64) -> Option<(i64, MappingKind)> {
    mapping.target(offset(cap)).map(|(o, kind)| (o.get(), kind))
}

/// Pass every step the mapping offers until it reaches `until`.
fn pass_until(mapping: &mut UnitMapping, until: i64, cap: i64) {
    while let Some((o, _)) = target(mapping, cap) {
        if o == until {
            return;
        }
        mapping.finish(StepResult::Passed);
    }
}

#[test]
fn a_new_ccd_has_an_unknown_edge_and_starts_at_stock() {
    let mapping = UnitMapping::default();
    assert_eq!(mapping.edge(), EdgeU::Unknown);
    assert_eq!(mapping.edge().offset(), None);
    assert_eq!(target(&mapping, 0), Some((0, MappingKind::Coarse)));
}

#[test]
fn coarse_descent_completes_a_step_before_going_five_counts_deeper() {
    let mut mapping = UnitMapping::default();
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(target(&mapping, 0).unwrap().0);
        mapping.finish(StepResult::Passed);
    }
    assert_eq!(seen, [0, -5, -10, -15]);
}

#[test]
fn the_first_failure_stops_the_step_and_brackets_from_the_last_passing_coarse_step() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -15, 0);
    mapping.finish(fail(9));
    assert_eq!(
        mapping.edge(),
        EdgeU::Observed {
            offset: offset(-15),
            witness: seq(9)
        }
    );
    let mut bracket = Vec::new();
    while let Some((o, MappingKind::Bracket)) = target(&mapping, 0) {
        bracket.push(o);
        mapping.finish(StepResult::Passed);
    }
    assert_eq!(bracket, [-11, -12, -13, -14]);
    // Every bracket step passed, so the coarse failure is U and the sweep follows.
    assert_eq!(mapping.edge().offset(), Some(offset(-15)));
    assert_eq!(mapping.cursor(), Cursor::Sweep { next: -14 });
}

#[test]
fn a_bracket_failure_is_the_shallowest_actual_failure_and_starts_the_sweep_above_it() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -15, 0);
    mapping.finish(fail(1));
    assert_eq!(target(&mapping, 0), Some((-11, MappingKind::Bracket)));
    mapping.finish(StepResult::Passed);
    mapping.finish(fail(2));
    assert_eq!(
        mapping.edge(),
        EdgeU::Observed {
            offset: offset(-12),
            witness: seq(2)
        }
    );
    let mut sweep = Vec::new();
    while let Some((o, MappingKind::Sweep)) = target(&mapping, -8) {
        sweep.push(o);
        mapping.finish(StepResult::Passed);
    }
    assert_eq!(sweep, [-11, -10, -9, -8]);
    assert_eq!(target(&mapping, -8), None);
}

#[test]
fn a_sweep_failure_moves_u_shallower_and_the_sweep_continues_above_it() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -10, 0);
    mapping.finish(fail(1));
    // Bracket -6..-9 passes.
    for _ in 0..4 {
        mapping.finish(StepResult::Passed);
    }
    assert_eq!(target(&mapping, -5), Some((-9, MappingKind::Sweep)));
    mapping.finish(StepResult::Passed);
    mapping.finish(fail(5));
    assert_eq!(
        mapping.edge(),
        EdgeU::Observed {
            offset: offset(-8),
            witness: seq(5)
        }
    );
    assert_eq!(target(&mapping, -5), Some((-7, MappingKind::Sweep)));
}

#[test]
fn a_shallower_sweep_extends_when_s_moves_up() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -10, 0);
    mapping.finish(fail(1));
    for _ in 0..4 {
        mapping.finish(StepResult::Passed);
    }
    for _ in 0..2 {
        mapping.finish(StepResult::Passed);
    }
    assert_eq!(target(&mapping, -8), None);
    // S moves shallower: the still-uncovered shallower cells are scheduled, nothing earlier repeats.
    assert_eq!(target(&mapping, -6), Some((-7, MappingKind::Sweep)));
}

#[test]
fn a_failure_at_stock_has_nothing_to_bracket() {
    let mut mapping = UnitMapping::default();
    mapping.finish(fail(1));
    assert_eq!(mapping.edge().offset(), Some(offset(0)));
    assert_eq!(target(&mapping, 0), None);
}

#[test]
fn the_full_matrix_passing_at_the_write_limit_censors_the_edge_at_minus_fifty() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -50, 0);
    assert_eq!(target(&mapping, 0), Some((-50, MappingKind::Coarse)));
    mapping.finish(StepResult::Passed);
    assert_eq!(mapping.edge(), EdgeU::Censored);
    assert_eq!(mapping.edge().offset(), Some(write_limit()));
    assert_eq!(write_limit().get(), -50);
    assert_eq!(mapping.cursor(), Cursor::Sweep { next: -49 });
}

#[test]
fn before_the_limit_without_a_failure_u_stays_unknown() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -45, 0);
    assert_eq!(mapping.edge(), EdgeU::Unknown);
}

#[test]
fn a_failure_after_a_censored_edge_replaces_it() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -50, 0);
    mapping.finish(StepResult::Passed);
    mapping.observe(offset(-30), charged(4));
    assert_eq!(
        mapping.edge(),
        EdgeU::Observed {
            offset: offset(-30),
            witness: seq(4)
        }
    );
}

#[test]
fn the_shallowest_failure_is_kept_even_after_a_later_pass_there() {
    let mut mapping = UnitMapping::default();
    mapping.observe(offset(-20), charged(1));
    mapping.observe(offset(-20), IsolatedResult::Pass);
    mapping.observe(offset(-25), charged(2));
    assert_eq!(
        mapping.edge(),
        EdgeU::Observed {
            offset: offset(-20),
            witness: seq(1)
        }
    );
    mapping.observe(offset(-15), charged(3));
    assert_eq!(
        mapping.edge(),
        EdgeU::Observed {
            offset: offset(-15),
            witness: seq(3)
        }
    );
}

#[test]
fn a_failure_located_off_the_tested_ccd_is_not_charged() {
    let mut mapping = UnitMapping::default();
    let before = mapping;
    mapping.observe(
        offset(-20),
        IsolatedResult::Fail {
            location: Location::Elsewhere,
            witness: seq(1),
        },
    );
    assert_eq!(mapping, before);
    assert_eq!(mapping.edge(), EdgeU::Unknown);
}

#[test]
fn an_outside_failure_inside_the_unmapped_gap_turns_the_descent_into_a_bracket() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -20, 0);
    mapping.observe(offset(-18), charged(7));
    assert_eq!(target(&mapping, 0), Some((-16, MappingKind::Bracket)));
    // A failure deeper than the step in progress leaves the descent alone.
    let mut deeper = UnitMapping::default();
    deeper.observe(offset(-30), charged(8));
    assert_eq!(target(&deeper, 0), Some((0, MappingKind::Coarse)));
}

#[test]
fn an_outside_failure_ends_a_bracket_and_sweeps_above_it() {
    let mut mapping = UnitMapping::default();
    pass_until(&mut mapping, -15, 0);
    mapping.finish(fail(1));
    mapping.observe(offset(-12), charged(2));
    assert_eq!(mapping.cursor(), Cursor::Sweep { next: -11 });
}

fn caps(entries: &[(u8, i64)]) -> BTreeMap<togi_tuner::unit::UnitId, TargetOffset> {
    entries
        .iter()
        .map(|&(id, counts)| (unit(id), offset(counts)))
        .collect()
}

#[test]
fn ccds_are_visited_round_robin_one_offset_each() {
    let mut board = MappingBoard::new([unit(0), unit(1)]);
    let mut visits = Vec::new();
    for _ in 0..5 {
        let target = board.next_target(&caps(&[])).unwrap();
        visits.push((target.unit, target.offset.get()));
        board.finish_step(target.unit, StepResult::Passed);
    }
    assert_eq!(
        visits,
        [
            (unit(0), 0),
            (unit(1), 0),
            (unit(0), -5),
            (unit(1), -5),
            (unit(0), -10)
        ]
    );
}

#[test]
fn a_ccd_with_nothing_to_map_is_skipped_and_the_rotation_wraps() {
    let mut board = MappingBoard::new([unit(0), unit(1), unit(2)]);
    // CCD 1 fails at stock: nothing to bracket, sweep range empty at S = 0.
    board.finish_step(unit(0), StepResult::Passed);
    board.finish_step(unit(1), fail(1));
    let next = board.next_target(&caps(&[])).unwrap();
    assert_eq!((next.unit, next.offset.get()), (unit(2), 0));
    board.finish_step(unit(2), StepResult::Passed);
    let next = board.next_target(&caps(&[])).unwrap();
    assert_eq!((next.unit, next.offset.get()), (unit(0), -5));
    assert_eq!(
        board.unit(unit(1)).unwrap().edge().offset(),
        Some(offset(0))
    );
}

#[test]
fn board_observations_reach_only_their_ccd() {
    let mut board = MappingBoard::new([unit(0), unit(1)]);
    board.observe(IsolatedObservation {
        unit: unit(1),
        offset: offset(-20),
        result: charged(3),
    });
    assert_eq!(board.unit(unit(0)).unwrap().edge(), EdgeU::Unknown);
    assert_eq!(
        board.unit(unit(1)).unwrap().edge().offset(),
        Some(offset(-20))
    );
}
