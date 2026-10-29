use super::super::{LAMBDA, NUMERICAL_TOLERANCE};
use super::*;
use crate::day_night::{SCENARIO_IDS as ORIGINAL_IDS, Scenario as OriginalScenario};
use test_case::test_case;

fn scenario(id: &str) -> Scenario {
    Scenario::builtin(id, 7).unwrap()
}

fn reference(scenario: &Scenario, day: u32, grouping: UnitGrouping) -> Reference {
    let calendar = ReferenceCalendar::standard(scenario).unwrap();
    Reference::new(
        scenario,
        day,
        grouping,
        &calendar,
        LAMBDA,
        NUMERICAL_TOLERANCE,
    )
    .unwrap()
}

fn core(index: usize) -> CoreId {
    CoreId::try_from(index).unwrap()
}

fn step(first: usize, rate: f64) -> Table {
    std::array::from_fn(|level| if level >= first { rate } else { 0.0 })
}

/// Synthetic tables: `tables[u]` is carried by the first core of unit `u`, the others are zero.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "the fixture transfers its root table into the owned reference"
)]
fn assembled(
    grouping: &UnitGrouping,
    tables: &[Table],
    pairs: Vec<Pair>,
    roots: Table,
) -> Reference {
    let mut rates = [None; CORES];
    for (group, table) in grouping.groups().iter().zip(tables) {
        for (index, core) in group.iter().enumerate() {
            rates[core.index()] = Some(if index == 0 { *table } else { [0.0; LEVELS] });
        }
    }
    Reference::assemble(
        grouping.clone(),
        rates,
        pairs,
        roots,
        LAMBDA,
        NUMERICAL_TOLERANCE,
    )
    .unwrap()
}

fn two_ccd() -> (Scenario, UnitGrouping) {
    let scenario = scenario("successor-core-interactions");
    let grouping = UnitGrouping::ccd(scenario.topology()).unwrap();
    assert_eq!(grouping.groups().len(), 2);
    (scenario, grouping)
}

/// Exhaustive optimum over unit-uniform profiles, judged by `admissibility` itself.
fn brute(reference: &Reference, cap: usize) -> u32 {
    let units = reference.grouping.groups.len();
    assert!((1..=2).contains(&units));
    let mut best = 0;
    for first in 0..=cap {
        for second in 0..=if units == 2 { cap } else { 0 } {
            let depths = if units == 2 {
                vec![first, second]
            } else {
                vec![first]
            };
            let profile = reference.profile_for(&depths).unwrap();
            if reference.admissibility(&profile).unwrap().member {
                best = best.max(u32::try_from(depths.iter().sum::<usize>()).unwrap());
            }
        }
    }
    best
}

#[test]
fn grouping_must_be_an_exact_partition() {
    let scenario = scenario("successor-core-interactions");
    let topology = scenario.topology();
    let ccd = UnitGrouping::ccd(topology).unwrap();
    assert_eq!(
        ccd.groups().iter().map(Vec::len).sum::<usize>(),
        topology.len()
    );
    let cores = UnitGrouping::cores(topology).unwrap();
    assert_eq!(cores.groups().len(), topology.len());
    assert!(cores.groups().iter().all(|group| group.len() == 1));

    let all = topology.iter().map(|info| info.core).collect::<Vec<_>>();
    let (head, tail) = all.split_at(8);
    assert!(UnitGrouping::new(topology, vec![head.to_vec(), tail.to_vec()]).is_ok());
    assert!(
        UnitGrouping::new(topology, vec![head.to_vec()]).is_err(),
        "missing cores"
    );
    assert!(
        UnitGrouping::new(topology, vec![all.clone(), vec![all[0]]]).is_err(),
        "duplicate core"
    );
    assert!(
        UnitGrouping::new(topology, vec![all.clone(), Vec::new()]).is_err(),
        "empty unit"
    );
    assert!(UnitGrouping::new(topology, vec![vec![core(15), core(14)], vec![core(8)]]).is_err());
    assert!(UnitGrouping::new(topology, Vec::new()).is_err());
    let small = super::super::Scenario::builtin("successor-single-ccd", 7).unwrap();
    assert!(
        Reference::new(
            &small,
            1,
            cores,
            &ReferenceCalendar::standard(&small).unwrap(),
            LAMBDA,
            NUMERICAL_TOLERANCE
        )
        .is_err(),
        "grouping of another topology"
    );
}

#[test]
fn calendar_validates_ordered_mix() {
    let hours = Duration::from_hours;
    assert!(ReferenceCalendar::new(hours(8), vec![0.25; 4]).is_ok());
    assert!(ReferenceCalendar::new(hours(24), vec![1.0]).is_ok());
    assert!(ReferenceCalendar::new(Duration::ZERO, vec![1.0]).is_err());
    assert!(ReferenceCalendar::new(hours(25), vec![1.0]).is_err());
    assert!(ReferenceCalendar::new(hours(8), Vec::new()).is_err());
    assert!(ReferenceCalendar::new(hours(8), vec![0.5, 0.4]).is_err());
    assert!(ReferenceCalendar::new(hours(8), vec![1.5, -0.5]).is_err());
    assert!(ReferenceCalendar::new(hours(8), vec![f64::NAN, 1.0]).is_err());
    let scenario = scenario("successor-edge-deep");
    let standard = ReferenceCalendar::standard(&scenario).unwrap();
    assert_eq!(standard.fractions().len(), scenario.daily_mix().len());
    assert_eq!(standard.active(), hours(8));
    let grouping = UnitGrouping::ccd(scenario.topology()).unwrap();
    let wrong = ReferenceCalendar::new(hours(8), vec![1.0]).unwrap();
    assert!(
        Reference::new(
            &scenario,
            1,
            grouping.clone(),
            &wrong,
            LAMBDA,
            NUMERICAL_TOLERANCE
        )
        .is_err()
    );
    let calendar = ReferenceCalendar::standard(&scenario).unwrap();
    assert!(
        Reference::new(
            &scenario,
            0,
            grouping.clone(),
            &calendar,
            LAMBDA,
            NUMERICAL_TOLERANCE
        )
        .is_err()
    );
    assert!(
        Reference::new(
            &scenario,
            1,
            grouping.clone(),
            &calendar,
            0.0,
            NUMERICAL_TOLERANCE
        )
        .is_err()
    );
    assert!(Reference::new(&scenario, 1, grouping, &calendar, LAMBDA, -1.0).is_err());
}

#[test]
fn separable_tables_have_analytic_optimum_and_cap_gap() {
    let (scenario, grouping) = two_ccd();
    drop(scenario);
    // Rate LAMBDA exceeds the per-unit limit LAMBDA / 2, so a unit stops at the step.
    let tables = [step(31, LAMBDA), step(49, LAMBDA)];
    let reference = assembled(&grouping, &tables, Vec::new(), [0.0; LEVELS]);
    let result = reference.depth_reference().unwrap();
    assert_eq!(result.physical.value, 30 + 48);
    assert_eq!(result.physical.upper_bound, 78);
    assert_eq!(result.operational.value, 30 + 45);
    assert_eq!(result.physical_gap, 3);
    assert_eq!(result.unit_maxima, vec![30, 45]);
    assert_eq!((result.operational.cap, result.physical.cap), (45, 50));
    assert_eq!(result.physical.components, 2);
}

#[test]
fn independent_components_share_the_joint_budget() {
    let (_, grouping) = two_ccd();
    let reference = assembled(
        &grouping,
        &[[0.0; LEVELS]; 2],
        vec![
            Pair {
                a: core(0),
                b: core(1),
                fraction: 1.0,
            },
            Pair {
                a: core(8),
                b: core(9),
                fraction: 1.0,
            },
        ],
        step(31, (0.75 * LAMBDA).sqrt()),
    );
    for depths in [[50, 0], [0, 50]] {
        let profile = reference.profile_for(&depths).unwrap();
        assert!(reference.admissibility(&profile).unwrap().member);
    }
    let both = reference.profile_for(&[50, 50]).unwrap();
    assert!(!reference.admissibility(&both).unwrap().member);

    let result = reference.depth_reference().unwrap();
    assert_eq!(result.physical.value, 80);
    assert_eq!(result.physical.upper_bound, 80);
    assert_eq!(result.operational.value, 75);
    assert_eq!(result.operational.upper_bound, 75);
    assert_eq!(result.physical_gap, 5);
    assert_eq!(result.physical.components, 2);
    assert_eq!(result.operational.components, 2);
    assert_eq!(result.unit_maxima, vec![45, 45]);
}

#[test]
fn rate_exactly_at_the_unit_limit_is_admitted() {
    let (_, grouping) = two_ccd();
    let reference = assembled(
        &grouping,
        &[step(10, LAMBDA / 2.0), step(10, LAMBDA / 2.0)],
        Vec::new(),
        [0.0; LEVELS],
    );
    assert_eq!(reference.depth_reference().unwrap().physical.value, 100);
    let reference = assembled(
        &grouping,
        &[step(10, LAMBDA / 2.0 * 1.001), step(0, 0.0)],
        Vec::new(),
        [0.0; LEVELS],
    );
    assert_eq!(
        reference.depth_reference().unwrap().unit_maxima,
        vec![9, 45]
    );
}

#[test]
fn step_interaction_couples_units() {
    let (scenario, grouping) = two_ccd();
    let topology = scenario.topology();
    assert_eq!(topology[0].ccd, topology[1].ccd);
    assert_ne!(topology[0].ccd, topology[8].ccd);
    let roots: Table = std::array::from_fn(|level| {
        if level > 20 {
            (4.0 * LAMBDA).sqrt()
        } else {
            0.0
        }
    });
    let zero = [[0.0; LEVELS]; 2];
    let cross = |fraction| {
        vec![Pair {
            a: core(0),
            b: core(8),
            fraction,
        }]
    };

    // Both above the step costs 4 * LAMBDA: only one unit may pass it.
    let result = assembled(&grouping, &zero, cross(1.0), roots)
        .depth_reference()
        .unwrap();
    assert_eq!(
        (
            result.physical.value,
            result.operational.value,
            result.physical_gap
        ),
        (70, 65, 5)
    );
    assert_eq!(result.unit_maxima, vec![45, 45]);
    // A quarter of the day costs exactly LAMBDA: admitted at the boundary.
    let result = assembled(&grouping, &zero, cross(0.25), roots)
        .depth_reference()
        .unwrap();
    assert_eq!(
        (
            result.physical.value,
            result.operational.value,
            result.physical_gap
        ),
        (100, 90, 10)
    );
    // Half the day costs 2 * LAMBDA.
    let result = assembled(&grouping, &zero, cross(0.5), roots)
        .depth_reference()
        .unwrap();
    assert_eq!(result.physical.value, 70);
    assert_eq!(result.physical.components, 1);
    assert!(result.physical.states > 0);

    // A pair inside one unit binds that unit alone.
    let inner = vec![Pair {
        a: core(0),
        b: core(1),
        fraction: 1.0,
    }];
    let result = assembled(&grouping, &zero, inner, roots)
        .depth_reference()
        .unwrap();
    assert_eq!(result.physical.value, 20 + 50);
    assert_eq!(result.unit_maxima, vec![20, 45]);
}

#[test]
fn coupled_component_shares_budget_with_separable_rates() {
    let (_, grouping) = two_ccd();
    let roots: Table = std::array::from_fn(|level| {
        if level > 20 {
            (4.0 * LAMBDA).sqrt()
        } else {
            0.0
        }
    });
    // Unit 0 carries rate LAMBDA / 4 from depth 30, leaving 3 LAMBDA / 4 for a 4 * fraction term.
    let tables = [step(30, LAMBDA / 4.0), [0.0; LEVELS]];
    let pairs = vec![Pair {
        a: core(0),
        b: core(8),
        fraction: 0.1875,
    }];
    let result = assembled(&grouping, &tables, pairs.clone(), roots)
        .depth_reference()
        .unwrap();
    // 0.1875 * 4 = 0.75 LAMBDA fits beside 0.25 LAMBDA; any higher rate would not.
    assert_eq!(result.physical.value, 100);
    let tables = [step(30, LAMBDA / 4.0 * 1.01), [0.0; LEVELS]];
    let result = assembled(&grouping, &tables, pairs, roots)
        .depth_reference()
        .unwrap();
    // Rate 1.01 * LAMBDA / 4 from depth 30 plus the 0.75 LAMBDA pair term overshoots LAMBDA,
    // so unit 0 stops at 29 while unit 1 keeps 50 (79) rather than 50 + 20 (70).
    assert_eq!(result.physical.value, 29 + 50);
}

#[test]
fn nonmonotone_or_infeasible_tables_are_refused() {
    let (_, grouping) = two_ccd();
    let mut rates = [None; CORES];
    for group in grouping.groups() {
        for core in group {
            rates[core.index()] = Some([0.0; LEVELS]);
        }
    }
    let mut falling = rates;
    falling[0] = Some(std::array::from_fn(
        |level| if level == 3 { 1.0 } else { 0.0 },
    ));
    assert!(
        Reference::assemble(
            grouping.clone(),
            falling,
            Vec::new(),
            [0.0; LEVELS],
            LAMBDA,
            0.0
        )
        .is_err()
    );
    let mut hot = rates;
    hot[0] = Some([LAMBDA; LEVELS]);
    let reference =
        Reference::assemble(grouping, hot, Vec::new(), [0.0; LEVELS], LAMBDA, 0.0).unwrap();
    assert!(
        reference.depth_reference().is_err(),
        "even depth zero is inadmissible"
    );
}

#[test_case("successor-edge-deep")]
#[test_case("successor-single-ccd")]
#[test_case("successor-thermal-history")]
#[test_case("successor-delayed-onset")]
#[test_case("successor-core-interactions")]
#[test_case("successor-both-ccd-interaction")]
#[test_case("successor-combined-adversary")]
fn matches_exhaustive_search_on_real_scenarios(id: &str) {
    let scenario = scenario(id);
    let reference = reference(
        &scenario,
        1,
        UnitGrouping::ccd(scenario.topology()).unwrap(),
    );
    let result = reference.depth_reference().unwrap();
    assert_eq!(
        result.physical.value,
        brute(&reference, 50),
        "{id} physical"
    );
    assert_eq!(
        result.operational.value,
        brute(&reference, 45),
        "{id} operational"
    );
    assert_eq!(result.physical.value, result.physical.upper_bound, "{id}");
    assert_eq!(
        result.operational.value, result.operational.upper_bound,
        "{id}"
    );
    assert_eq!(
        result.physical_gap,
        result.physical.value - result.operational.value,
        "{id}"
    );
    assert_eq!(result.unit_maxima.len(), reference.grouping.groups.len());
}

#[test_case(1)]
#[test_case(46)]
#[test_case(181)]
fn legacy_fixtures_reproduce_the_original_safe_profile(day: u32) {
    for (base, id) in ORIGINAL_IDS.iter().zip(super::super::SCENARIO_IDS.iter()) {
        let original = OriginalScenario::builtin(base, 7).unwrap();
        let scenario = scenario(id);
        let reference = reference(
            &scenario,
            day,
            UnitGrouping::ccd(scenario.topology()).unwrap(),
        );
        let expected = original
            .safe_profile(day - 1, LAMBDA)
            .unwrap()
            .iter()
            .map(|offset| u32::try_from(-offset.get()).unwrap())
            .sum::<u32>();
        assert_eq!(
            reference.depth_reference().unwrap().physical.value,
            expected,
            "{id} day {day}"
        );
    }
}

#[test]
fn legacy_interaction_uses_its_root_product() {
    let scenario = scenario("successor-both-ccd-interaction");
    let reference = reference(
        &scenario,
        1,
        UnitGrouping::ccd(scenario.topology()).unwrap(),
    );
    assert_eq!(reference.pairs.len(), 1);
    assert_eq!(
        (reference.pairs[0].a, reference.pairs[0].b),
        (core(0), core(8))
    );
    assert!(reference.roots[50] > 0.0);
    assert_eq!(reference.roots[0], 0.0);
}

#[test]
fn heterogeneous_cores_and_ccds_have_distinct_optima() {
    let scenario = scenario("successor-within-ccd-heterogeneous");
    let topology = scenario.topology();
    let by_core = reference(&scenario, 1, UnitGrouping::cores(topology).unwrap())
        .depth_reference()
        .unwrap();
    let by_ccd = reference(&scenario, 1, UnitGrouping::ccd(topology).unwrap())
        .depth_reference()
        .unwrap();
    // Weak cores stop at 20 and strong at 40; a CCD must hold its weakest core.
    assert_eq!(by_core.physical.value, 8 * 20 + 8 * 40);
    assert_eq!(by_ccd.physical.value, 2 * 20);
    assert_eq!(by_core.physical_gap, 0);
}

#[test]
fn refinement_truth_exceeds_the_registered_coarse_result() {
    let scenario = scenario("successor-core-refinement");
    let topology = scenario.topology();
    let by_core = reference(&scenario, 1, UnitGrouping::cores(topology).unwrap())
        .depth_reference()
        .unwrap();
    let by_ccd = reference(&scenario, 1, UnitGrouping::ccd(topology).unwrap())
        .depth_reference()
        .unwrap();
    assert_eq!(by_core.physical.value, 8 * 40 + 8 * 44);
    assert_eq!(by_core.operational.value, by_core.physical.value);
    assert_eq!(by_ccd.physical.value, 2 * 40);
}

#[test]
fn nonuniform_profiles_are_judged_independently_of_grouping() {
    let scenario = scenario("successor-within-ccd-heterogeneous");
    let topology = scenario.topology();
    let by_core = reference(&scenario, 1, UnitGrouping::cores(topology).unwrap());
    let by_ccd = reference(&scenario, 1, UnitGrouping::ccd(topology).unwrap());
    let build = |depth_of: &dyn Fn(usize) -> u8| {
        let depths = topology
            .iter()
            .map(|info| (info.core, Depth::new(depth_of(info.core.index())).unwrap()))
            .collect::<PerCore<Depth>>();
        Profile::new(depths, topology).unwrap()
    };
    // Two asymmetric admissible profiles: no core passes its own failure edge.
    let ramp = build(&|index| u8::try_from(5 + index).unwrap());
    let alternating = build(&|index| if index % 2 == 0 { 20 } else { 3 });
    for profile in [&ramp, &alternating] {
        for reference in [&by_core, &by_ccd] {
            let verdict = reference.admissibility(profile).unwrap();
            assert!(verdict.member, "{verdict:?}");
            assert!(verdict.joint <= LAMBDA && verdict.joint_ratio <= 1.0);
            assert_eq!(verdict.tolerance, NUMERICAL_TOLERANCE);
            assert_eq!(verdict.per_unit.len(), reference.grouping.groups.len());
        }
    }
    assert_ne!(ramp, alternating);

    // Nonuniform and inadmissible: one strong core past its edge, one weak core far past its own.
    let stressed = build(&|index| match index {
        0 => 48,
        9 => 43,
        _ => 0,
    });
    let cores = by_core.admissibility(&stressed).unwrap();
    let ccds = by_ccd.admissibility(&stressed).unwrap();
    assert!((cores.joint - ccds.joint).abs() < 1e-12);
    assert!((cores.per_unit.iter().sum::<f64>() - ccds.per_unit.iter().sum::<f64>()).abs() < 1e-12);
    assert_eq!(cores.per_unit_ratios.len(), 16);
    assert_eq!(ccds.per_unit_ratios.len(), 2);
    assert!(!cores.member);
    assert!(cores.per_unit_ratios.iter().any(|ratio| *ratio > 1.0));

    let deep = Profile::uniform(topology, Depth::new(50).unwrap());
    assert!(!by_core.admissibility(&deep).unwrap().member);
    // Profiles over another topology are refused rather than guessed.
    let other = super::super::Scenario::builtin("successor-single-ccd", 7).unwrap();
    let foreign = Profile::uniform(other.topology(), Depth::new(1).unwrap());
    assert!(by_core.admissibility(&foreign).is_err());
}

#[test]
fn optimum_profile_is_admitted_and_one_deeper_is_not() {
    let scenario = scenario("successor-edge-shallow");
    let topology = scenario.topology();
    let reference = reference(&scenario, 1, UnitGrouping::ccd(topology).unwrap());
    let result = reference.depth_reference().unwrap();
    let units = reference.grouping.groups.len();
    let maxima = result
        .unit_maxima
        .iter()
        .map(|level| usize::from(*level))
        .collect::<Vec<_>>();
    // Without coupling the optimum is every unit at its own maximum.
    assert_eq!(
        result.physical.value,
        u32::try_from(maxima.iter().sum::<usize>()).unwrap()
    );
    assert!(
        reference
            .admissibility(&reference.profile_for(&maxima).unwrap())
            .unwrap()
            .member
    );
    for unit in 0..units {
        if maxima[unit] < 50 {
            let mut deeper = maxima.clone();
            deeper[unit] += 1;
            assert!(
                !reference
                    .admissibility(&reference.profile_for(&deeper).unwrap())
                    .unwrap()
                    .member
            );
        }
    }
}

#[test]
fn catalog_epoch_references_are_certified() {
    for id in super::super::SCENARIO_IDS {
        let scenario = Scenario::builtin(id, 20001).unwrap();
        let mut epochs = BTreeSet::new();
        let days = (0..=10).map(|step| 1 + 45 * step).chain([181]);
        for day in days {
            if !epochs.insert(scenario.epoch(day).unwrap().index) {
                continue;
            }
            for grouping in [
                UnitGrouping::ccd(scenario.topology()).unwrap(),
                UnitGrouping::cores(scenario.topology()).unwrap(),
            ] {
                let units = grouping.groups().len();
                let reference = reference(&scenario, day, grouping);
                let result = reference.depth_reference().unwrap();
                for certificate in [result.operational, result.physical] {
                    assert_eq!(
                        certificate.value, certificate.upper_bound,
                        "{id} day {day} units {units}"
                    );
                    assert!(
                        certificate.value
                            <= u32::from(certificate.cap) * u32::try_from(units).unwrap()
                    );
                }
                assert_eq!(
                    result.physical_gap,
                    result.physical.value - result.operational.value,
                    "{id} day {day}"
                );
                assert_eq!(result.unit_maxima.len(), units);
            }
        }
    }
}

#[test_case("successor-weak-core-hidden")]
#[test_case("successor-weak-core-delayed")]
fn weak_core_objectives_have_an_analytic_bound(id: &str) {
    let scenario = scenario(id);
    let by_core = reference(
        &scenario,
        1,
        UnitGrouping::cores(scenario.topology()).unwrap(),
    )
    .depth_reference()
    .unwrap();
    let by_ccd = reference(
        &scenario,
        1,
        UnitGrouping::ccd(scenario.topology()).unwrap(),
    )
    .depth_reference()
    .unwrap();
    assert_eq!(by_core.operational.value, 2 * 20 + 14 * 40);
    assert_eq!(by_core.operational.upper_bound, by_core.operational.value);
    assert_eq!(by_ccd.operational.value, 40);
    assert_eq!(by_ccd.operational.upper_bound, 40);
}

#[test]
fn interaction_objective_and_two_asymmetric_profiles_are_not_tie_broken() {
    let scenario = scenario("successor-core-interactions");
    let reference = reference(
        &scenario,
        1,
        UnitGrouping::cores(scenario.topology()).unwrap(),
    );
    let result = reference.depth_reference().unwrap();
    // Dropping core 0 by 20 breaks both interactions, a minimum-weight vertex cover.
    assert_eq!(result.operational.value, 620);
    assert_eq!(result.operational.upper_bound, 620);
    let make = |shallow: &[usize]| {
        Profile::new(
            scenario
                .topology()
                .iter()
                .map(|info| {
                    (
                        info.core,
                        Depth::new(if shallow.contains(&info.core.index()) {
                            20
                        } else {
                            40
                        })
                        .unwrap(),
                    )
                })
                .collect(),
            scenario.topology(),
        )
        .unwrap()
    };
    let first = make(&[0]);
    let second = make(&[1, 8]);
    assert_ne!(first, second);
    assert!(reference.admissibility(&first).unwrap().member);
    assert!(reference.admissibility(&second).unwrap().member);
    let unsafe_profile = make(&[]);
    let verdict = reference.admissibility(&unsafe_profile).unwrap();
    assert!(!verdict.member);
    assert_eq!(verdict.interactions.len(), 2);
    assert_eq!(
        verdict.joint,
        verdict
            .interactions
            .iter()
            .map(|source| source.per_hour)
            .sum::<f64>()
    );
}

#[test]
fn combined_objective_has_an_analytic_vertex_cover_bound() {
    let scenario = scenario("successor-combined-adversary");
    let reference = reference(
        &scenario,
        1,
        UnitGrouping::cores(scenario.topology()).unwrap(),
    );
    let already_broken = scenario.weak.contains(&core(0))
        || (scenario.weak.contains(&core(1)) && scenario.weak.contains(&core(8)));
    let expected = 600 - if already_broken { 0 } else { 20 };
    let result = reference.depth_reference().unwrap();
    assert_eq!(result.operational.value, expected);
    assert_eq!(result.operational.upper_bound, expected);
}
