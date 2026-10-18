use std::num::NonZeroU32;

use proptest::prelude::*;
use test_case::test_case;
use togi_machine::{
    CcdId, CoreId, CoreInfo, CoreSet, PerCore, ResetKind, ResetReason, TargetOffset,
};

use crate::{
    CombinationId, ConstraintError, Constraints, CrashFacts, CrashKind, Depth, FailurePoint,
    Insertion, Limit, Profile, ProfileError, Ranking, RankingError, Reached, Topology,
    TopologyError, TopologyMismatch, classify_crash, recorded_kind,
};

pub(crate) fn core(id: u8) -> CoreId {
    CoreId::new(i64::from(id)).unwrap()
}

pub(crate) fn off(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

fn cid(id: u32) -> CombinationId {
    CombinationId::new(NonZeroU32::new(id).unwrap())
}

fn info(id: u8, ccd: u8) -> CoreInfo {
    CoreInfo {
        core: core(id),
        ccd: CcdId::try_from(ccd).unwrap(),
        cpus: Vec::new(),
    }
}

pub(crate) fn topology(ids: &[u8]) -> Topology {
    let infos: Vec<CoreInfo> = ids.iter().map(|&id| info(id, id / 8)).collect();
    Topology::new(&infos).unwrap()
}

pub(crate) fn profile(topology: &Topology, values: &[i64]) -> Profile {
    let offsets: PerCore<TargetOffset> = topology
        .all()
        .iter()
        .zip(values)
        .map(|(core, &value)| (core, off(value)))
        .collect();
    Profile::new(topology, offsets).unwrap()
}

fn members(pairs: &[(u8, i64)]) -> PerCore<TargetOffset> {
    pairs
        .iter()
        .map(|&(id, value)| (core(id), off(value)))
        .collect()
}

fn constraints(
    topology: &Topology,
    failures: &[(u8, i64)],
    combinations: &[&[(u8, i64)]],
) -> Constraints {
    let mut constraints = Constraints::new(topology);
    for &(id, value) in failures {
        constraints
            .insert_failure_point(core(id), FailurePoint::new(off(value)))
            .unwrap();
    }
    for (index, pairs) in combinations.iter().enumerate() {
        let id = cid(u32::try_from(index).unwrap() + 1);
        constraints.insert_combination(id, members(pairs)).unwrap();
    }
    constraints
}

fn ranking(topology: &Topology, ids: &[u8]) -> Ranking {
    Ranking::new(topology, ids.iter().map(|&id| core(id)).collect()).unwrap()
}

fn exhaustive(constraints: &Constraints, hi: &Profile, ranking: &Ranking) -> Option<Profile> {
    let cores: Vec<CoreId> = hi.cores().iter().collect();
    let floor = off(-50);
    let mut p = Profile::over(hi.cores(), |_| floor);
    let mut best: Option<Profile> = None;
    loop {
        if constraints.reaches(&p) == Ok(None)
            && best
                .as_ref()
                .is_none_or(|best| ranked_better(&p, best, ranking))
        {
            best = Some(p);
        }
        let mut position = 0;
        loop {
            let Some(&current) = cores.get(position) else {
                return best;
            };
            if p.get(current) == hi.get(current) {
                p = p.with(current, floor);
                position += 1;
            } else {
                p = p.with(current, off(p.get(current).unwrap().get() + 1));
                break;
            }
        }
    }
}

fn ranked_better(p: &Profile, best: &Profile, ranking: &Ranking) -> bool {
    if p.total() != best.total() {
        return p.total() < best.total();
    }
    ranking
        .order()
        .iter()
        .find(|&&core| p.get(core) != best.get(core))
        .is_some_and(|&core| p.get(core).unwrap().deeper_than(best.get(core).unwrap()))
}

#[test]
fn depth_orders_and_steps_offsets() {
    assert!(off(-5).deeper_than(off(-4)));
    assert!(off(-4).shallower_than(off(-5)));
    assert!(!off(-4).deeper_than(off(-4)));
    assert_eq!(off(-3).shallower_by(5), off(0));
    assert_eq!(off(-10).shallower_by(5), off(-5));
    assert_eq!(off(-48).deeper_by(2), Some(off(-50)));
    assert_eq!(off(-48).deeper_by(3), None);
    assert_eq!(off(-48).deeper_or_floor(3), off(-50));
    assert_eq!(off(-10).deeper_or_floor(5), off(-15));
    assert!(off(-50).at_floor() && !off(-49).at_floor());
    assert_eq!(off(-7).signed(), -7);
}

#[test]
fn topology_alternates_ccds_and_accepts_unequal_sizes() {
    let topology =
        Topology::new(&[info(9, 1), info(0, 0), info(2, 0), info(8, 1), info(1, 0)]).unwrap();
    let order: Vec<u8> = topology.order().iter().map(|core| core.get()).collect();
    assert_eq!(order, [0, 8, 1, 9, 2]);
    assert_eq!(topology.ccd_of(core(9)), Some(CcdId::new(1).unwrap()));
    assert_eq!(topology.ccd_of(core(3)), None);
    let ccds: Vec<(u8, Vec<u8>)> = topology
        .ccds()
        .iter()
        .map(|(ccd, set)| (ccd.get(), set.iter().map(CoreId::get).collect()))
        .collect();
    assert_eq!(ccds, [(0, vec![0, 1, 2]), (1, vec![8, 9])]);
    assert_eq!(topology.all().len(), 5);
    let parts: Vec<usize> = topology.parts().iter().map(CoreSet::len).collect();
    assert_eq!(parts, [3, 2, 5]);
}

#[test]
fn single_ccd_topology_has_the_ccd_as_its_only_part() {
    let topology = topology(&[0, 1, 2]);
    assert_eq!(topology.parts(), [topology.all()]);
}

#[test]
fn topology_rejects_empty_duplicate_and_four_ccds() {
    assert_eq!(Topology::new(&[]), Err(TopologyError::Empty));
    assert_eq!(
        Topology::new(&[info(0, 0), info(0, 1)]),
        Err(TopologyError::DuplicateCore(core(0)))
    );
    let four: Vec<CoreInfo> = (0..4).map(|ccd| info(ccd * 2, ccd)).collect();
    assert_eq!(Topology::new(&four), Err(TopologyError::TooManyCcds(4)));
    let three: Vec<CoreInfo> = (0..3).map(|ccd| info(ccd * 2, ccd)).collect();
    assert!(Topology::new(&three).is_ok());
}

#[test]
fn ranking_must_be_a_permutation_of_the_topology() {
    let topology = topology(&[0, 4, 9]);
    assert!(Ranking::new(&topology, vec![core(9), core(0), core(4)]).is_ok());
    assert_eq!(
        Ranking::new(&topology, vec![core(0), core(4)]),
        Err(RankingError::Missing(core(9)))
    );
    assert_eq!(
        Ranking::new(&topology, vec![core(0), core(4), core(4), core(9)]),
        Err(RankingError::Duplicate(core(4)))
    );
    assert_eq!(
        Ranking::new(&topology, vec![core(0), core(1), core(4), core(9)]),
        Err(RankingError::Unknown(core(1)))
    );
    let ascending = Ranking::ascending(&topology);
    let order: Vec<u8> = ascending.order().iter().map(|core| core.get()).collect();
    assert_eq!(order, [0, 4, 9]);
}

#[test]
fn profile_must_cover_exactly_the_topology() {
    let topology = topology(&[0, 8]);
    let missing: PerCore<TargetOffset> = [(core(0), off(-1))].into_iter().collect();
    assert_eq!(
        Profile::new(&topology, missing),
        Err(ProfileError::Missing(core(8)))
    );
    let extra: PerCore<TargetOffset> = [(core(0), off(-1)), (core(8), off(-2)), (core(3), off(0))]
        .into_iter()
        .collect();
    assert_eq!(
        Profile::new(&topology, extra),
        Err(ProfileError::Extra(core(3)))
    );
}

#[test]
fn profile_alone_zero_with_and_total() {
    let topology = topology(&[0, 1, 8]);
    let zero = Profile::zero(&topology);
    assert!(zero.is_zero());
    assert_eq!(zero.sole_nonzero(), None);
    let alone = Profile::alone(&topology, core(8), off(-12));
    assert!(!alone.is_zero());
    assert_eq!(alone.get(core(8)), Some(off(-12)));
    assert_eq!(alone.get(core(0)), Some(off(0)));
    assert_eq!(alone.total(), -12);
    let moved = alone.with(core(1), off(-3));
    assert_eq!(moved.total(), -15);
    assert_eq!(alone.total(), -12);
    assert_eq!(moved.with(core(5), off(-9)), moved);
    assert_eq!(moved.get(core(5)), None);
}

#[test]
fn profile_covers_and_within_compare_every_core() {
    let topology = topology(&[0, 1]);
    let deep = profile(&topology, &[-20, -10]);
    let shallow = profile(&topology, &[-10, -10]);
    let mixed = profile(&topology, &[-30, -5]);
    assert!(deep.covers(&shallow));
    assert!(!shallow.covers(&deep));
    assert!(shallow.within(&deep));
    assert!(!deep.within(&shallow));
    assert!(deep.covers(&deep) && deep.within(&deep));
    assert!(!mixed.covers(&deep) && !mixed.within(&deep));
}

#[test]
fn constraints_reach_and_at_limit() {
    let topology = topology(&[0, 1]);
    let constraints = constraints(&topology, &[(0, -50)], &[&[(0, -49), (1, -50)]]);

    assert_eq!(
        constraints.reaches(&profile(&topology, &[-49, -49])),
        Ok(None)
    );
    let failure = constraints
        .reaches(&profile(&topology, &[-50, -49]))
        .unwrap()
        .unwrap();
    assert_eq!(failure.to_string(), "failure point -50 of core 00");
    assert!(matches!(failure, Reached::FailurePoint { core: c, .. } if c == core(0)));
    let combination = constraints
        .reaches(&profile(&topology, &[-49, -50]))
        .unwrap()
        .unwrap();
    assert_eq!(combination, Reached::Combination(cid(1)));
    assert_eq!(combination.to_string(), "combination C1");

    let limit = constraints
        .at_limit(core(0), &profile(&topology, &[-49, -49]))
        .unwrap()
        .unwrap();
    assert!(matches!(
        limit,
        Limit::Reached(Reached::FailurePoint { .. })
    ));
    let limit = constraints
        .at_limit(core(1), &profile(&topology, &[-49, -49]))
        .unwrap()
        .unwrap();
    assert_eq!(limit, Limit::Reached(Reached::Combination(cid(1))));
    let floor = constraints
        .at_limit(core(1), &profile(&topology, &[-49, -50]))
        .unwrap()
        .unwrap();
    assert_eq!(floor, Limit::Floor);
    assert_eq!(floor.to_string(), "at the floor -50");
    assert_eq!(
        constraints.at_limit(core(1), &profile(&topology, &[-10, -10])),
        Ok(None)
    );
}

#[test]
fn constraints_check_failure_points_before_combinations_in_core_order() {
    let topology = topology(&[0, 1, 2]);
    let constraints = constraints(
        &topology,
        &[(2, -10), (1, -10)],
        &[&[(0, -20)], &[(0, -30)]],
    );
    let all_deep = profile(&topology, &[-40, -40, -40]);
    assert!(matches!(
        constraints.reaches(&all_deep),
        Ok(Some(Reached::FailurePoint { core: c, .. })) if c == core(1)
    ));
    let only_combinations = profile(&topology, &[-40, -5, -5]);
    assert_eq!(
        constraints.reaches(&only_combinations),
        Ok(Some(Reached::Combination(cid(1))))
    );
}

#[test_case(-30, -20, -20; "new shallower point")]
#[test_case(-20, -40, -20; "prior shallower point")]
#[test_case(-20, -20, -20; "equal points")]
#[test_case(-50, 0, 0; "zero point")]
fn failure_point_keeps_the_shallowest(prior: i64, observed: i64, expected: i64) {
    let topology = topology(&[0]);
    let mut constraints = Constraints::new(&topology);
    constraints
        .insert_failure_point(core(0), FailurePoint::new(off(prior)))
        .unwrap();
    constraints
        .insert_failure_point(core(0), FailurePoint::new(off(observed)))
        .unwrap();
    assert_eq!(
        constraints.failure_points().get(core(0)),
        Some(FailurePoint::new(off(expected)))
    );
    assert_eq!(
        constraints.insert_failure_point(core(1), FailurePoint::new(off(0))),
        Err(ConstraintError::UnknownCore(core(1)))
    );
}

#[test]
fn duplicate_member_sets_are_not_inserted_twice() {
    let topology = topology(&[0, 1, 2]);
    let mut constraints = Constraints::new(&topology);
    let pair = members(&[(0, -10), (1, -20)]);
    assert_eq!(
        constraints.insert_combination(cid(1), pair),
        Ok(Insertion::Added)
    );
    assert_eq!(
        constraints.insert_combination(cid(2), pair),
        Ok(Insertion::Duplicate(cid(1)))
    );
    assert_eq!(
        constraints.insert_combination(cid(3), members(&[(0, -10), (1, -21)])),
        Ok(Insertion::Added)
    );
    assert_eq!(
        constraints.insert_combination(cid(3), members(&[(2, -1)])),
        Err(ConstraintError::DuplicateId(cid(3)))
    );
    assert_eq!(
        constraints.insert_combination(cid(4), PerCore::default()),
        Err(ConstraintError::EmptyCombination)
    );
    assert_eq!(
        constraints.insert_combination(cid(4), members(&[(5, -1)])),
        Err(ConstraintError::UnknownCore(core(5)))
    );
    let ids: Vec<u32> = constraints
        .combinations()
        .iter()
        .map(|c| c.id().get())
        .collect();
    assert_eq!(ids, [1, 3]);
}

#[test]
fn clear_core_drops_its_point_and_combinations_and_keeps_the_rest() {
    let topology = topology(&[0, 1, 2]);
    let mut constraints = constraints(
        &topology,
        &[(0, -10), (1, -10)],
        &[
            &[(0, -5), (1, -5)],
            &[(1, -6), (2, -6)],
            &[(0, -7), (2, -7)],
            &[(1, -8)],
        ],
    );
    assert_eq!(constraints.combinations_with(core(0)).count(), 2);
    let cleared = constraints.clear_core(core(0));
    assert_eq!(cleared, [cid(1), cid(3)]);
    assert_eq!(constraints.failure_points().get(core(0)), None);
    assert!(constraints.failure_points().get(core(1)).is_some());
    let kept: Vec<u32> = constraints
        .combinations()
        .iter()
        .map(|c| c.id().get())
        .collect();
    assert_eq!(kept, [2, 4]);
    let with_one: Vec<u32> = constraints
        .combinations_with(core(1))
        .map(|c| c.id().get())
        .collect();
    assert_eq!(with_one, [2, 4]);
    assert_eq!(constraints.combinations_with(core(0)).count(), 0);
    assert!(constraints.clear_core(core(0)).is_empty());
}

#[test]
fn together_crash_names_the_sole_nonzero_core_by_id() {
    let topology = topology(&[0, 8]);
    let applied = profile(&topology, &[0, -12]);
    assert_eq!(applied.sole_nonzero(), Some(core(8)));
    let three = self::topology(&[0, 1, 2]);
    assert_eq!(profile(&three, &[0, -10, 0]).sole_nonzero(), Some(core(1)));
    assert_eq!(profile(&three, &[-1, -1, 0]).sole_nonzero(), None);
    assert_eq!(profile(&three, &[0, 0, 0]).sole_nonzero(), None);
}

#[test]
fn optimum() {
    struct Case {
        name: &'static str,
        pairs: &'static [(u8, u8)],
        hi: &'static [i64],
        ranking: &'static [u8],
        want: &'static [i64],
    }
    let cases = [
        Case {
            name: "two individually legal deepenings form a combination",
            pairs: &[(0, 1)],
            hi: &[0, 0],
            ranking: &[0, 1],
            want: &[-50, -49],
        },
        Case {
            name: "three-core backoff counterexample",
            pairs: &[(0, 1), (0, 2)],
            hi: &[-49, -49, -50],
            ranking: &[0, 1, 2],
            want: &[-49, -50, -50],
        },
        Case {
            name: "four-core cycle global",
            pairs: &[(0, 1), (1, 2), (2, 3), (3, 0), (1, 3)],
            hi: &[0, 0, 0, 0],
            ranking: &[0, 1, 2, 3],
            want: &[-50, -49, -50, -49],
        },
        Case {
            name: "better-ranked equal-sum later",
            pairs: &[(0, 1)],
            hi: &[0, 0],
            ranking: &[1, 0],
            want: &[-49, -50],
        },
    ];
    for case in cases {
        let ids: Vec<u8> = (0..u8::try_from(case.hi.len()).unwrap()).collect();
        let topology = topology(&ids);
        let combinations: Vec<[(u8, i64); 2]> = case
            .pairs
            .iter()
            .map(|&(a, b)| [(a, -50), (b, -50)])
            .collect();
        let refs: Vec<&[(u8, i64)]> = combinations.iter().map(<[_; 2]>::as_slice).collect();
        let constraints = constraints(&topology, &[], &refs);
        let got = constraints
            .optimum(
                &profile(&topology, case.hi),
                &ranking(&topology, case.ranking),
            )
            .unwrap();
        assert_eq!(got, Some(profile(&topology, case.want)), "{}", case.name);
    }
}

#[test]
fn optimum_with_staircase_combinations() {
    let topology = topology(&[0, 1, 2]);
    let mut step = [-26, -38, -29];
    let mut constraints = constraints(&topology, &[(0, -27), (1, -39), (2, -30)], &[]);
    for k in 0..39_u32 {
        let pairs = [(0, step[0]), (1, step[1]), (2, step[2])];
        constraints
            .insert_combination(cid(k + 1), members(&pairs))
            .unwrap();
        step[2 - usize::try_from(k % 3).unwrap()] += 1;
    }
    let hi = Profile::zero(&topology);
    let ranking = ranking(&topology, &[0, 1, 2]);
    let want = exhaustive(&constraints, &hi, &ranking);
    assert!(want.is_some());
    assert_eq!(constraints.optimum(&hi, &ranking).unwrap(), want);
}

#[test]
fn four_core_cycle_needs_global_yield() {
    let topology = topology(&[0, 1, 2, 3]);
    let pairs: Vec<[(u8, i64); 2]> = [(0, 1), (1, 2), (2, 3), (3, 0), (1, 3)]
        .iter()
        .map(|&(a, b)| [(a, -50), (b, -50)])
        .collect();
    let refs: Vec<&[(u8, i64)]> = pairs.iter().map(<[_; 2]>::as_slice).collect();
    let constraints = constraints(&topology, &[], &refs);
    let ascending = Ranking::ascending(&topology);
    let global = constraints
        .optimum(&Profile::zero(&topology), &ascending)
        .unwrap()
        .unwrap();
    assert_eq!(global.total(), -198);
    for bounded in [[-49, -49, -49, -50], [-49, -50, -49, -49]] {
        let target = constraints
            .optimum(&profile(&topology, &bounded), &ascending)
            .unwrap()
            .unwrap();
        assert_eq!(target.total(), -197, "{bounded:?} yields {target:?}");
    }
}

#[test]
fn optimum_keeps_failure_point_outside_safe_bounds() {
    let topology = topology(&[0]);
    let constraints = constraints(&topology, &[(0, -11)], &[]);
    let ranking = ranking(&topology, &[0]);
    for hi in [-10, 0] {
        assert_eq!(
            constraints.optimum(&profile(&topology, &[hi]), &ranking),
            Ok(Some(profile(&topology, &[-10])))
        );
    }
}

#[test]
fn optimum_is_none_when_the_floor_passes_the_ceiling_or_a_combination_cannot_break() {
    let topology = topology(&[0, 1]);
    let ranking = Ranking::ascending(&topology);
    let at_zero = constraints(&topology, &[(0, 0)], &[]);
    assert_eq!(
        at_zero.optimum(&Profile::zero(&topology), &ranking),
        Ok(None)
    );
    let above = constraints(&topology, &[(0, -10)], &[]);
    assert_eq!(
        above.optimum(&profile(&topology, &[-11, 0]), &ranking),
        Ok(None)
    );
    let unbreakable = constraints(&topology, &[], &[&[(0, -10), (1, -10)]]);
    assert_eq!(
        unbreakable.optimum(&profile(&topology, &[-10, -10]), &ranking),
        Ok(None)
    );
    assert!(
        unbreakable
            .optimum(&profile(&topology, &[-9, -10]), &ranking)
            .unwrap()
            .is_some()
    );
}

#[test]
fn constraints_reject_a_profile_of_other_cores() {
    let topology = topology(&[0, 1]);
    let other = self::topology(&[4, 5]);
    let smaller = self::topology(&[0]);
    let constraints = constraints(&topology, &[(0, -50)], &[&[(0, -50), (1, -50)]]);
    let ranking = Ranking::ascending(&topology);
    for foreign in [Profile::zero(&other), Profile::zero(&smaller)] {
        let mismatch = TopologyMismatch {
            expected: topology.all(),
            found: foreign.cores(),
        };
        assert_eq!(constraints.reaches(&foreign), Err(mismatch));
        assert_eq!(constraints.at_limit(core(0), &foreign), Err(mismatch));
        assert_eq!(constraints.optimum(&foreign, &ranking), Err(mismatch));
    }
    let foreign_ranking = Ranking::ascending(&other);
    assert!(
        constraints
            .optimum(&Profile::zero(&topology), &foreign_ranking)
            .is_err()
    );
}

#[test]
fn profile_comparison_never_invents_a_core() {
    let topology = topology(&[0, 1]);
    let other = self::topology(&[4, 5]);
    let here = Profile::zero(&topology);
    let there = Profile::zero(&other);
    assert_eq!(here.get(core(4)), None);
    assert!(!here.covers(&there) && !here.within(&there));
    assert!(!there.covers(&here) && !there.within(&here));
}

fn classify(facts: &CrashFacts) -> CrashKind {
    let kind = classify_crash(facts);
    let recorded = recorded_kind(
        kind == CrashKind::Stray,
        kind == CrashKind::Inconclusive || kind == CrashKind::Thermal,
        &facts.reason,
        facts.in_trial,
    );
    assert_eq!(recorded, kind, "recorded kind of {facts:?}");
    kind
}

fn reason(kind: Option<ResetKind>, supported: bool) -> ResetReason {
    ResetReason {
        kind,
        raw: Vec::new(),
        supported,
    }
}

fn nothing(reason: ResetReason) -> CrashFacts {
    CrashFacts {
        in_trial: false,
        applied: false,
        evidence: false,
        reason,
        confirmed: false,
    }
}

#[test]
fn classify_crash_cases() {
    let none = reason(None, false);
    let thermal = reason(Some(ResetKind::ThermalTrip), true);
    let button = reason(Some(ResetKind::PowerButton), true);
    let supported = reason(None, true);
    let cases = [
        (
            "trial",
            CrashFacts {
                in_trial: true,
                ..nothing(none.clone())
            },
            CrashKind::InTrial,
        ),
        (
            "idle",
            CrashFacts {
                applied: true,
                ..nothing(none.clone())
            },
            CrashKind::Idle,
        ),
        ("stray", nothing(none.clone()), CrashKind::Stray),
        (
            "evidence before thermal",
            CrashFacts {
                applied: true,
                evidence: true,
                ..nothing(thermal.clone())
            },
            CrashKind::Idle,
        ),
        (
            "thermal",
            CrashFacts {
                in_trial: true,
                ..nothing(thermal)
            },
            CrashKind::Thermal,
        ),
        (
            "power button trial",
            CrashFacts {
                in_trial: true,
                ..nothing(button.clone())
            },
            CrashKind::InTrial,
        ),
        (
            "power button idle",
            CrashFacts {
                applied: true,
                ..nothing(button)
            },
            CrashKind::Inconclusive,
        ),
        (
            "confirmed absent reason",
            CrashFacts {
                applied: true,
                confirmed: true,
                ..nothing(supported.clone())
            },
            CrashKind::Inconclusive,
        ),
        (
            "unsupported absent reason",
            CrashFacts {
                applied: true,
                confirmed: true,
                ..nothing(none)
            },
            CrashKind::Idle,
        ),
        (
            "unconfirmed absent reason",
            CrashFacts {
                applied: true,
                ..nothing(supported)
            },
            CrashKind::Idle,
        ),
        (
            "unknown",
            nothing(reason(Some(ResetKind::Unknown), true)),
            CrashKind::Stray,
        ),
    ];
    for (name, facts, want) in cases {
        assert_eq!(classify(&facts), want, "{name}");
    }
}

#[test]
fn other_reset_kinds_classify_by_what_was_applied() {
    for kind in [
        ResetKind::Watchdog,
        ResetKind::SyncFlood,
        ResetKind::CpuShutdown,
        ResetKind::PowerLoss,
    ] {
        let confirmed = CrashFacts {
            confirmed: true,
            ..nothing(reason(Some(kind), true))
        };
        assert_eq!(
            classify(&CrashFacts {
                in_trial: true,
                ..confirmed.clone()
            }),
            CrashKind::InTrial
        );
        assert_eq!(
            classify(&CrashFacts {
                applied: true,
                ..confirmed.clone()
            }),
            CrashKind::Idle
        );
        assert_eq!(classify(&confirmed), CrashKind::Stray);
    }
}

#[test]
fn thermal_reason_yields_to_mce_evidence() {
    let thermal = reason(Some(ResetKind::ThermalTrip), true);
    let with_evidence = CrashFacts {
        evidence: true,
        ..nothing(thermal.clone())
    };
    assert_eq!(
        classify(&CrashFacts {
            in_trial: true,
            applied: true,
            ..with_evidence.clone()
        }),
        CrashKind::InTrial
    );
    assert_eq!(
        classify(&CrashFacts {
            applied: true,
            ..with_evidence
        }),
        CrashKind::Idle
    );
    assert_eq!(
        recorded_kind(false, false, &thermal, true),
        CrashKind::InTrial
    );
    assert_eq!(
        recorded_kind(false, true, &thermal, true),
        CrashKind::Thermal
    );
}

const IDS: [u8; 5] = [0, 3, 4, 9, 15];

#[derive(Debug)]
struct Scenario {
    cores: usize,
    failures: Vec<Option<i64>>,
    combinations: Vec<Vec<Option<i64>>>,
    hi: Vec<i64>,
    permutation: Vec<usize>,
    ranked: bool,
}

fn scenarios() -> impl Strategy<Value = Scenario> {
    (2_usize..=5).prop_flat_map(|cores| {
        (
            Just(cores),
            prop::collection::vec(prop::option::of(-50_i64..=-47), cores),
            prop::collection::vec(
                (
                    prop::collection::vec(prop::option::of(-50_i64..=-45), cores),
                    -50_i64..=-45,
                ),
                1..=40,
            ),
            prop::collection::vec(-47_i64..=-42, cores),
            Just((0..cores).collect::<Vec<_>>()).prop_shuffle(),
            prop::bool::weighted(0.75),
        )
            .prop_map(|(cores, failures, combinations, hi, permutation, ranked)| {
                let combinations = combinations
                    .into_iter()
                    .map(|(mut offsets, fill)| {
                        let mut present = offsets.iter().flatten().count();
                        for slot in &mut offsets {
                            if present >= 2 {
                                break;
                            }
                            if slot.is_none() {
                                *slot = Some(fill);
                                present += 1;
                            }
                        }
                        offsets
                    })
                    .collect();
                Scenario {
                    cores,
                    failures,
                    combinations,
                    hi,
                    permutation,
                    ranked,
                }
            })
    })
}

proptest! {
    #![proptest_config(crate::prop_tests::seeded(500))]

    #[test]
    fn optimum_matches_exhaustive_search(scenario in scenarios()) {
        let ids = &IDS[..scenario.cores];
        let topology = topology(ids);
        let failures: Vec<(u8, i64)> = ids
            .iter()
            .zip(&scenario.failures)
            .filter_map(|(&id, point)| point.map(|point| (id, point)))
            .collect();
        let combinations: Vec<Vec<(u8, i64)>> = scenario
            .combinations
            .iter()
            .map(|offsets| {
                ids.iter()
                    .zip(offsets)
                    .filter_map(|(&id, offset)| offset.map(|offset| (id, offset)))
                    .collect()
            })
            .collect();
        let refs: Vec<&[(u8, i64)]> = combinations.iter().map(Vec::as_slice).collect();
        let constraints = constraints(&topology, &failures, &refs);
        let hi = profile(&topology, &scenario.hi);
        let ranking = if scenario.ranked {
            let ranked: Vec<u8> = scenario.permutation.iter().map(|&index| ids[index]).collect();
            ranking(&topology, &ranked)
        } else {
            Ranking::ascending(&topology)
        };
        let got = constraints.optimum(&hi, &ranking).unwrap();
        prop_assert_eq!(&got, &exhaustive(&constraints, &hi, &ranking));
        if let Some(found) = got {
            prop_assert_eq!(constraints.reaches(&found), Ok(None));
            prop_assert!(found.covers(&hi));
        }
    }
}

#[test]
fn recent_names_the_core_of_the_latest_shallower_point_or_the_latest_combination() {
    let topology = topology(&[0, 1, 2]);
    let mut constraints = Constraints::new(&topology);
    assert!(constraints.recent().is_empty());
    constraints
        .insert_failure_point(core(1), FailurePoint::new(off(-20)))
        .unwrap();
    assert_eq!(constraints.recent().iter().collect::<Vec<_>>(), [core(1)]);
    constraints
        .insert_failure_point(core(2), FailurePoint::new(off(-30)))
        .unwrap();
    constraints
        .insert_failure_point(core(1), FailurePoint::new(off(-25)))
        .unwrap();
    assert_eq!(constraints.recent().iter().collect::<Vec<_>>(), [core(2)]);
    constraints
        .insert_combination(cid(1), members(&[(0, -10), (1, -10)]))
        .unwrap();
    assert_eq!(
        constraints.recent().iter().collect::<Vec<_>>(),
        [core(0), core(1)]
    );
    constraints
        .insert_combination(cid(2), members(&[(2, -5)]))
        .unwrap();
    constraints
        .insert_combination(cid(3), members(&[(2, -5)]))
        .unwrap();
    assert_eq!(constraints.recent().iter().collect::<Vec<_>>(), [core(2)]);
}

#[test]
fn a_failure_point_can_be_replaced_or_cleared_but_only_for_a_core_of_the_topology() {
    let topology = topology(&[0]);
    let mut constraints = Constraints::new(&topology);
    let point = |value| Some(FailurePoint::new(off(value)));
    constraints.set_failure_point(core(0), point(-20)).unwrap();
    constraints.set_failure_point(core(0), point(-30)).unwrap();
    assert_eq!(constraints.failure_points().get(core(0)), point(-30));
    constraints.set_failure_point(core(0), None).unwrap();
    assert_eq!(constraints.failure_points().get(core(0)), None);
    assert_eq!(
        constraints.set_failure_point(core(1), point(-1)),
        Err(ConstraintError::UnknownCore(core(1)))
    );
}

#[test]
fn repeating_a_constraint_does_not_add_to_what_the_optimum_solves() {
    let topology = topology(&[0, 1, 2, 3]);
    let pairs: [&[(u8, i64)]; 3] = [
        &[(0, -50), (1, -50)],
        &[(1, -50), (2, -50)],
        &[(2, -50), (3, -50)],
    ];
    let once = constraints(&topology, &[], &pairs);
    let mut repeated = once.clone();
    for round in 0..1000_u32 {
        for (index, pair) in pairs.iter().enumerate() {
            let id = cid(100 + round * 3 + u32::try_from(index).unwrap());
            assert_eq!(
                repeated.insert_combination(id, members(pair)),
                Ok(Insertion::Duplicate(cid(u32::try_from(index).unwrap() + 1)))
            );
        }
    }
    assert_eq!(repeated.combinations().len(), pairs.len());
    let ranking = Ranking::ascending(&topology);
    let hi = Profile::zero(&topology);
    assert_eq!(
        repeated.optimum(&hi, &ranking).unwrap(),
        once.optimum(&hi, &ranking).unwrap()
    );
}
