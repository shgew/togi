use std::collections::{BTreeSet, HashSet};

use test_case::test_case;
use togi_machine::{ALL_REGIMES, CcdId, CpuId, PerCore, Regime, WorkloadId, pick_workload};
use togi_tuner::night::{
    AttemptScope, Cell, HostPool, Matrix, PackageLoad, Purpose, Reuse, Roles, Source, Workloads,
    control_cells, mapping_cells, minimum_position_seconds, qualification_cells,
    reproduction_cells, zeroed,
};
use togi_tuner::{HonestChain, UncoveredReason};

use crate::fixture::{ccd, core, machine, offset, profile, set};

fn regime_cells(matrix: &Matrix, regime: Regime) -> Vec<Cell> {
    matrix
        .cells
        .iter()
        .map(|planned| planned.cell)
        .filter(|cell| cell.regime() == regime)
        .collect()
}

fn regime_order(matrix: &Matrix) -> Vec<Regime> {
    let mut order: Vec<Regime> = matrix.cells.iter().map(|p| p.cell.regime()).collect();
    order.dedup();
    order
}

fn two_ccds() -> crate::fixture::Machine {
    machine(2, 8)
}

/// Unchanged CCD 1's R1–R5 cells of an all-fresh qualification: the witnesses a prior step left.
fn unchanged_witnesses(machine: &crate::fixture::Machine) -> HashSet<Cell> {
    let all =
        qualification_cells(&machine.layout(), &profile(&[-5, -5]), 0, &Reuse::AllFresh).unwrap();
    all.cells
        .iter()
        .map(|planned| planned.cell)
        .filter(|cell| {
            let per_core = matches!(
                cell.regime(),
                Regime::R1 | Regime::R2 | Regime::R3 | Regime::R4 | Regime::R5
            );
            per_core
                && cell
                    .position()
                    .is_some_and(|c| machine.topology.ccd_of(c) == Some(ccd(1)))
        })
        .collect()
}

#[test]
fn a_one_ccd_step_on_two_eight_core_ccds_is_155_launches_and_19380_seconds() {
    let machine = two_ccds();
    let witnesses = unchanged_witnesses(&machine);
    let changed = BTreeSet::from([ccd(0)]);
    let matrix = qualification_cells(
        &machine.layout(),
        &profile(&[-10, -5]),
        0,
        &Reuse::Unchanged {
            changed: &changed,
            witnesses: &witnesses,
        },
    )
    .unwrap();
    assert_eq!(matrix.fresh_launches(), 155);
    assert_eq!(matrix.fresh_seconds(), 19_380);
}

#[test]
fn every_cell_runs_fresh_without_reuse_marks() {
    let machine = two_ccds();
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-10, -5]), 0, &Reuse::AllFresh).unwrap();
    // 155 plus CCD 1's 104 R1-R5 cells.
    assert_eq!(matrix.fresh_launches(), 259);
    assert_eq!(matrix.cells.len(), 259);
}

#[test]
fn unchanged_cells_are_reused_only_with_a_witness_and_changed_cells_never() {
    let machine = two_ccds();
    let mut witnesses = unchanged_witnesses(&machine);
    let kept = *witnesses.iter().find(|c| c.regime() == Regime::R1).unwrap();
    witnesses.retain(|cell| *cell != kept);
    let changed_cell = machine_cell(&machine, ccd(0));
    witnesses.insert(changed_cell);
    let changed = BTreeSet::from([ccd(0)]);
    let matrix = qualification_cells(
        &machine.layout(),
        &profile(&[-10, -5]),
        0,
        &Reuse::Unchanged {
            changed: &changed,
            witnesses: &witnesses,
        },
    )
    .unwrap();
    let source = |cell: Cell| matrix.cells.iter().find(|p| p.cell == cell).unwrap().source;
    assert_eq!(source(kept), Source::Fresh);
    assert_eq!(source(changed_cell), Source::Fresh);
    assert_eq!(matrix.fresh_launches(), 155 + 1);
}

fn machine_cell(machine: &crate::fixture::Machine, ccd: CcdId) -> Cell {
    let all =
        qualification_cells(&machine.layout(), &profile(&[-5, -5]), 0, &Reuse::AllFresh).unwrap();
    all.cells
        .iter()
        .map(|p| p.cell)
        .find(|cell| {
            cell.regime() == Regime::R2
                && cell
                    .position()
                    .is_some_and(|c| machine.topology.ccd_of(c) == Some(ccd))
        })
        .unwrap()
}

#[test]
fn qualification_order_is_r1_through_r9() {
    let machine = two_ccds();
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5, -5]), 0, &Reuse::AllFresh).unwrap();
    assert_eq!(regime_order(&matrix), ALL_REGIMES);
}

#[test_case(Regime::R1, 24, 90, 0)]
#[test_case(Regime::R2, 24, 90, 0)]
#[test_case(Regime::R3, 24, 120, 0)]
#[test_case(Regime::R4, 24, 120, 0)]
#[test_case(Regime::R5, 8, 120, 0)]
#[test_case(Regime::R6, 1, 900, 0)]
#[test_case(Regime::R7, 1 + 6, 120, 1; "R7 single CCD: full plus six fallback partials")]
#[test_case(Regime::R8, 8, 120, 1)]
#[test_case(Regime::R9, 8, 120, 1)]
fn single_ccd_rows(regime: Regime, breadth: usize, secs: u32, sustained: usize) {
    let machine = machine(1, 8);
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5]), 0, &Reuse::AllFresh).unwrap();
    let cells = regime_cells(&matrix, regime);
    let (long, short): (Vec<&Cell>, Vec<&Cell>) = cells.iter().partition(|cell| cell.sustained);
    assert_eq!(short.len(), breadth, "{regime:?} breadth");
    assert!(short.iter().all(|cell| cell.duration.get() == secs));
    assert_eq!(long.len(), sustained, "{regime:?} sustained");
    assert!(long.iter().all(|cell| cell.duration.get() == 600));
}

#[test_case(Regime::R1, 24, 90, 0)]
#[test_case(Regime::R2, 24, 90, 0)]
#[test_case(Regime::R3, 24, 120, 0)]
#[test_case(Regime::R4, 24, 120, 0)]
#[test_case(Regime::R5, 8, 120, 0)]
#[test_case(Regime::R6, 1, 900, 0)]
#[test_case(Regime::R7, 15, 120, 1; "R7 two full, twelve partials, all-CCD full")]
#[test_case(Regime::R8, 16, 120, 1)]
#[test_case(Regime::R9, 16, 120, 1)]
fn one_ccd_step_rows(regime: Regime, breadth: usize, secs: u32, sustained: usize) {
    let machine = two_ccds();
    let witnesses = unchanged_witnesses(&machine);
    let changed = BTreeSet::from([ccd(0)]);
    let matrix = qualification_cells(
        &machine.layout(),
        &profile(&[-10, -5]),
        0,
        &Reuse::Unchanged {
            changed: &changed,
            witnesses: &witnesses,
        },
    )
    .unwrap();
    let fresh: Vec<Cell> = matrix
        .fresh()
        .copied()
        .filter(|c| c.regime() == regime)
        .collect();
    let short = fresh.iter().filter(|cell| !cell.sustained).count();
    assert_eq!(short, breadth, "{regime:?} breadth");
    assert!(
        fresh
            .iter()
            .filter(|c| !c.sustained)
            .all(|c| c.duration.get() == secs)
    );
    assert_eq!(
        fresh.iter().filter(|cell| cell.sustained).count(),
        sustained
    );
}

#[test]
fn r3_and_r4_use_the_first_three_entries_paired_with_duties() {
    let machine = machine(1, 8);
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5]), 0, &Reuse::AllFresh).unwrap();
    let entries = |regime| -> Vec<usize> {
        let ids: Vec<WorkloadId> = regime_cells(&matrix, regime)
            .iter()
            .map(|cell| match cell.workloads {
                Workloads::One { workload } => workload,
                Workloads::LightHeavy { light, .. } => light,
            })
            .collect();
        let mut distinct = ids;
        distinct.dedup();
        distinct
            .iter()
            .map(|id| {
                (0..9)
                    .position(|i| WorkloadId::new(regime, i) == Some(*id))
                    .unwrap()
            })
            .collect()
    };
    assert_eq!(entries(Regime::R3), [0, 1, 2]);
    assert_eq!(entries(Regime::R4), [0, 1, 2]);
    // Catalog outer, core inner: the first eight cells share the first entry.
    let r1 = regime_cells(&matrix, Regime::R1);
    assert!(r1[..8].iter().all(|cell| cell.workloads == r1[0].workloads));
    assert_eq!(
        r1[..8]
            .iter()
            .map(|cell| cell.position().unwrap())
            .collect::<Vec<_>>(),
        (0..8).map(core).collect::<Vec<_>>()
    );
}

#[test]
fn ordinals_rotate_the_selected_entries_and_positions() {
    let machine = two_ccds();
    let at = |ordinal| {
        qualification_cells(
            &machine.layout(),
            &profile(&[-5, -5]),
            ordinal,
            &Reuse::AllFresh,
        )
        .unwrap()
    };
    let first = |matrix: &Matrix, regime| regime_cells(matrix, regime)[0];
    let (zero, one, two) = (at(0), at(1), at(2));
    // R5 enumerates R1 then R2 entries; R6 and R7 rotate their catalogs.
    for (matrix, index) in [(&zero, 0), (&one, 1), (&at(7), 1)] {
        assert_eq!(
            first(matrix, Regime::R5).workloads,
            Workloads::One {
                workload: pick_workload(Regime::R5, index).id()
            }
        );
    }
    assert_eq!(
        first(&one, Regime::R6).workloads,
        Workloads::One {
            workload: pick_workload(Regime::R6, 1).id()
        }
    );
    // R8: sustained light then bursty light per index-matched pair.
    let pair = |matrix: &Matrix| first(matrix, Regime::R8).workloads;
    let expect = |light, heavy| Workloads::LightHeavy {
        light: pick_workload(Regime::R8, light).id(),
        heavy: pick_workload(Regime::R2, heavy).id(),
    };
    assert_eq!(pair(&zero), expect(0, 0));
    assert_eq!(pair(&one), expect(3, 0));
    assert_eq!(pair(&two), expect(1, 1));
    assert_eq!(pair(&at(6)), expect(0, 0));
    // R9 picks the serial SMT sibling by ordinal modulo the siblings.
    let serial = |matrix: &Matrix| match first(matrix, Regime::R9).roles {
        Roles::AllThreads { serial, .. } => serial,
        Roles::SingleCore { .. }
        | Roles::SmtPair { .. }
        | Roles::AllCoresIdle
        | Roles::Package(_)
        | Roles::Neighbours { .. } => panic!("R9 cell without a serial CPU"),
    };
    assert_eq!(serial(&zero), CpuId::new(0).unwrap());
    assert_eq!(serial(&one), CpuId::new(32).unwrap());
    // Sustained R8/R9 position is the ordinal modulo the physical-core count.
    let sustained = |matrix: &Matrix, regime| {
        regime_cells(matrix, regime)
            .into_iter()
            .find(|c| c.sustained)
            .unwrap()
            .position()
            .unwrap()
    };
    assert_eq!(sustained(&zero, Regime::R8), core(0));
    assert_eq!(sustained(&at(5), Regime::R8), core(5));
    assert_eq!(sustained(&at(17), Regime::R9), core(1));
}

#[test]
fn package_rows_run_full_ccds_then_partials_then_all_ccds_and_a_sustained_block() {
    let machine = two_ccds();
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5, -5]), 0, &Reuse::AllFresh).unwrap();
    let roles: Vec<Roles> = regime_cells(&matrix, Regime::R7)
        .iter()
        .map(|c| c.roles)
        .collect();
    assert_eq!(
        roles[0],
        Roles::Package(PackageLoad::FullCcd { ccd: ccd(0) })
    );
    assert_eq!(
        roles[1],
        Roles::Package(PackageLoad::FullCcd { ccd: ccd(1) })
    );
    assert!(
        roles[2..8]
            .iter()
            .all(|r| *r == Roles::Package(PackageLoad::Partial { ccd: ccd(0) }))
    );
    assert!(
        roles[8..14]
            .iter()
            .all(|r| *r == Roles::Package(PackageLoad::Partial { ccd: ccd(1) }))
    );
    assert_eq!(roles[14], Roles::Package(PackageLoad::AllCcds));
    assert_eq!(roles[15], Roles::Package(PackageLoad::AllCcds));
}

#[test]
fn a_single_ccd_has_one_full_cell_and_its_sustained_block_is_the_full_ccd() {
    let machine = machine(1, 8);
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5]), 0, &Reuse::AllFresh).unwrap();
    let roles: Vec<Roles> = regime_cells(&matrix, Regime::R7)
        .iter()
        .map(|c| c.roles)
        .collect();
    let full = Roles::Package(PackageLoad::FullCcd { ccd: ccd(0) });
    assert_eq!(roles.iter().filter(|r| **r == full).count(), 2);
    assert!(!roles.contains(&Roles::Package(PackageLoad::AllCcds)));
}

#[test]
fn partials_come_from_the_honest_chain_and_uncovered_loads_are_typed() {
    let mut machine = machine(2, 8);
    // A measured chain with two partials for CCD 0; CCD 1 has only two cores' worth of chain.
    machine.partials.insert(
        ccd(0),
        HonestChain::Partials {
            loaded: set(&[0, 1, 2, 3, 4, 5, 6, 7]),
            basis: togi_tuner::PartialBasis::Measured,
            partials: vec![set(&[1, 2, 3, 4, 5, 6, 7]), set(&[2, 3])],
        },
    );
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5, -5]), 0, &Reuse::AllFresh).unwrap();
    let partial_loads: Vec<_> = regime_cells(&matrix, Regime::R7)
        .into_iter()
        .filter(|c| c.roles == Roles::Package(PackageLoad::Partial { ccd: ccd(0) }))
        .map(|c| c.loaded)
        .collect();
    assert_eq!(partial_loads, [set(&[1, 2, 3, 4, 5, 6, 7]), set(&[2, 3])]);
    assert!(matrix.uncovered.is_empty());
}

#[test]
fn fewer_than_three_cores_is_an_uncovered_reason_not_a_partial() {
    let machine = machine(2, 2);
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5, -5]), 0, &Reuse::AllFresh).unwrap();
    assert_eq!(matrix.uncovered.len(), 2);
    assert!(
        matrix
            .uncovered
            .iter()
            .all(|u| u.reason == UncoveredReason::TooFewLoaded)
    );
    // Two fulls and the all-CCD cell; no partial cell stands in for coverage.
    assert_eq!(
        regime_cells(&matrix, Regime::R7)
            .iter()
            .filter(|c| !c.sustained)
            .count(),
        3
    );
}

#[test]
fn a_tied_measured_chain_without_a_partial_runs_the_subset_fallback() {
    let mut machine = machine(1, 3);
    let loaded = set(&[0, 1, 2]);
    let requests: PerCore<f64> = [(core(0), 1.2), (core(1), 1.2), (core(2), 1.0)]
        .into_iter()
        .collect();
    let chain = HonestChain::derive(loaded, &requests);
    assert!(!chain.is_coverage());
    machine.partials.insert(ccd(0), chain);
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5]), 0, &Reuse::AllFresh).unwrap();
    let partials: Vec<_> = regime_cells(&matrix, Regime::R7)
        .into_iter()
        .filter(|c| matches!(c.roles, Roles::Package(PackageLoad::Partial { .. })))
        .map(|c| c.loaded)
        .collect();
    assert_eq!(partials, [set(&[0, 1])]);
    assert!(matrix.uncovered.is_empty());
}

#[test]
fn control_cells_are_the_full_matrix_at_all_zero_fresh() {
    let machine = two_ccds();
    let matrix = control_cells(&machine.layout(), &profile(&[-10, -5]), 3).unwrap();
    assert_eq!(matrix.profile, profile(&[0, 0]));
    assert_eq!(matrix.fresh_launches(), 259);
    assert!(matrix.cells.iter().all(|p| p.source == Source::Fresh));
}

#[test]
fn mapping_runs_r1_r2_r8_r7_r9_on_one_ccd_with_the_others_at_zero() {
    let machine = two_ccds();
    let matrix = mapping_cells(&machine.layout(), crate::fixture::unit(1), offset(-15), 0).unwrap();
    assert_eq!(matrix.profile, profile(&[0, -15]));
    assert_eq!(
        regime_order(&matrix),
        [Regime::R1, Regime::R2, Regime::R8, Regime::R7, Regime::R9]
    );
    let count = |regime| regime_cells(&matrix, regime).len();
    assert_eq!(
        [Regime::R1, Regime::R2, Regime::R8, Regime::R7, Regime::R9].map(count),
        [24, 24, 8, 7, 1]
    );
    assert!(
        matrix
            .cells
            .iter()
            .all(|p| p.cell.purpose == Purpose::Isolated && !p.cell.sustained)
    );
    assert!(matrix.cells.iter().all(|p| {
        p.cell
            .loaded
            .iter()
            .all(|c| machine.topology.ccd_of(c) == Some(ccd(1)))
    }));
    let r9 = regime_cells(&matrix, Regime::R9)[0];
    assert_eq!(r9.duration.get(), 120);
}

#[test]
fn mapping_selections_are_frozen_by_the_mapping_ordinal() {
    let machine = two_ccds();
    let at = |ordinal| {
        mapping_cells(
            &machine.layout(),
            crate::fixture::unit(0),
            offset(-5),
            ordinal,
        )
        .unwrap()
    };
    let pair = |matrix: &Matrix| regime_cells(matrix, Regime::R8)[0].workloads;
    assert_ne!(pair(&at(0)), pair(&at(1)));
    assert_eq!(pair(&at(1)), pair(&at(7)));
    let serial_core = |matrix: &Matrix| regime_cells(matrix, Regime::R9)[0].position().unwrap();
    assert_eq!(serial_core(&at(3)), core(3));
}

#[test]
fn a_core_unit_maps_its_own_positions_under_its_whole_ccds_package_loads() {
    let machine = two_ccds();
    let layout = machine.layout();
    let unit = togi_tuner::UnitId::Core(core(9));
    let matrix = mapping_cells(&layout, unit, offset(-15), 0).unwrap();
    let coarse = mapping_cells(&layout, crate::fixture::unit(1), offset(-15), 0).unwrap();
    assert_eq!(matrix.profile.get(unit), Some(offset(-15)));
    assert!(
        matrix
            .cells
            .iter()
            .filter_map(|planned| planned.cell.position())
            .all(|position| position == core(9))
    );
    assert_eq!(
        regime_cells(&matrix, Regime::R7),
        regime_cells(&coarse, Regime::R7)
    );
    assert_eq!(matrix.uncovered, coarse.uncovered);
    let all_threads = regime_cells(&matrix, Regime::R9)[0];
    assert_eq!(all_threads.loaded, set(&[8, 9, 10, 11, 12, 13, 14, 15]));
}

#[test]
fn reproduction_tests_the_exact_profile_then_each_nonzero_ccd_alone() {
    let machine = two_ccds();
    let attempts = reproduction_cells(&machine.layout(), &profile(&[-10, -5]), 0).unwrap();
    let scopes: Vec<_> = attempts.iter().map(|a| a.scope).collect();
    assert_eq!(
        scopes,
        [
            AttemptScope::Exact,
            AttemptScope::Unit {
                unit: crate::fixture::unit(0)
            },
            AttemptScope::Unit {
                unit: crate::fixture::unit(1)
            }
        ]
    );
    assert_eq!(attempts[0].matrix.profile, profile(&[-10, -5]));
    assert_eq!(attempts[1].matrix.profile, profile(&[-10, 0]));
    assert_eq!(attempts[2].matrix.profile, profile(&[0, -5]));
    assert_eq!(regime_order(&attempts[0].matrix), ALL_REGIMES);
    assert_eq!(attempts[0].matrix.fresh_launches(), 259);
    // An isolated attempt tests its CCD only, with no all-CCD package cell; R6 keeps the exact
    // placement contract.
    let isolated = &attempts[1].matrix;
    assert!(
        !isolated
            .cells
            .iter()
            .any(|p| p.cell.roles == Roles::Package(PackageLoad::AllCcds))
    );
    assert!(
        isolated
            .cells
            .iter()
            .filter(|p| p.cell.regime() != Regime::R6)
            .all(|p| p.cell.purpose == Purpose::Isolated)
    );
    assert!(
        isolated
            .cells
            .iter()
            .filter(|p| p.cell.regime() == Regime::R6)
            .all(|p| p.cell.purpose == Purpose::Exact)
    );
}

#[test]
fn a_ccd_at_zero_has_nothing_to_isolate() {
    let machine = two_ccds();
    let attempts = reproduction_cells(&machine.layout(), &profile(&[-10, 0]), 0).unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[1].scope,
        AttemptScope::Unit {
            unit: crate::fixture::unit(0)
        }
    );
}

#[test]
fn zeroed_clears_every_ccd() {
    assert_eq!(zeroed(&profile(&[-10, -5])), profile(&[0, 0]));
}

#[test]
fn eligible_seconds_exclude_the_warm_up_inside_the_duration() {
    let machine = machine(1, 4);
    let matrix =
        qualification_cells(&machine.layout(), &profile(&[-5]), 0, &Reuse::AllFresh).unwrap();
    let eligible = |regime| regime_cells(&matrix, regime)[0].eligible_seconds();
    assert_eq!(eligible(Regime::R1), 85);
    assert_eq!(eligible(Regime::R6), 895);
    assert_eq!(eligible(Regime::R8), 90);
    assert_eq!(eligible(Regime::R9), 90);
}

#[test]
fn a_single_ccd_with_an_unloaded_core_hosts_there_and_credits_every_position() {
    let pool = HostPool::single_ccd(set(&[0, 1, 2, 3]), set(&[0]));
    assert_eq!(pool, HostPool::Unloaded(set(&[1, 2, 3])));
    assert_eq!(pool.host(0), Some(core(1)));
    assert_eq!(pool.host(4), Some(core(2)));
    assert!((0..6).all(|ordinal| pool.credits(ordinal, core(0))));
}

#[test]
fn with_every_core_loaded_the_host_rotates_and_its_position_earns_no_credit() {
    let pool = HostPool::single_ccd(set(&[0, 1, 2, 3]), set(&[0, 1, 2, 3]));
    assert_eq!(pool, HostPool::Loaded(set(&[0, 1, 2, 3])));
    assert_eq!(pool.host(2), Some(core(2)));
    assert!(!pool.credits(2, core(2)));
    assert!(pool.credits(3, core(2)));
}

fn machine_one_cell() -> Cell {
    let machine = machine(1, 4);
    qualification_cells(&machine.layout(), &profile(&[-5]), 0, &Reuse::AllFresh)
        .unwrap()
        .cells[0]
        .cell
}

#[test]
fn a_cell_does_not_credit_the_position_its_host_occupies() {
    let cell = machine_one_cell();
    let position = cell.position().unwrap();
    assert!(!cell.credits_position(position));
    assert!(cell.credits_position(core(3)));
}

#[test]
fn position_accounting_uses_the_minimum_across_required_positions() {
    let credited: PerCore<u32> = [(core(0), 300), (core(1), 120), (core(2), 600)]
        .into_iter()
        .collect();
    assert_eq!(minimum_position_seconds(set(&[0, 1, 2]), &credited), 120);
    assert_eq!(minimum_position_seconds(set(&[0, 3]), &credited), 0);
    assert_eq!(minimum_position_seconds(set(&[]), &credited), 0);
}
