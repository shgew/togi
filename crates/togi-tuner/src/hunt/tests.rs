use std::collections::HashSet;
use std::num::NonZeroU32;

use proptest::prelude::*;
use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use super::plan::{
    Backoff, Duration, End, Found, Group, GroupOutcome, GroupStage, Located, MemberProbes, Plan,
    PlanError, Probe, ProbeResult, ProbeStep, Start, already_broken, combination_backoff,
    group_profile, park, park_located, partition,
};
use crate::tests::{core, off, profile, topology};
use crate::{CombinationId, Constraints, Depth, FailurePoint, Profile, Ranking};
use togi_journal::{HuntOutcome, Seq};

fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

fn ids(set: CoreSet) -> Vec<u8> {
    set.iter().map(CoreId::get).collect()
}

fn seq(n: u64) -> Seq {
    Seq::new(n).unwrap()
}

fn start(candidates: &[u8]) -> Start {
    Start {
        candidates: set(candidates),
        loaded: set(candidates),
        recent: CoreSet::default(),
        longer_failure: false,
        duration_prior: false,
        located: None,
        repeated_core: None,
    }
}

fn located(candidates: &[u8], loaded: &[u8], loaded_at_zero: bool) -> Start {
    Start {
        loaded: set(loaded),
        located: Some(Located { loaded_at_zero }),
        ..start(candidates)
    }
}

fn drive(plan: &mut Plan, mut outcome: impl FnMut(&Group) -> GroupOutcome) -> Vec<Group> {
    let mut groups = Vec::new();
    while let Some(group) = plan.next_group() {
        assert!(groups.len() < 1000, "plan did not terminate");
        let outcome = outcome(&group);
        groups.push(group);
        plan.advance(outcome);
    }
    groups
}

fn passing(_: &Group) -> GroupOutcome {
    GroupOutcome::Passed
}

fn fails_when_holding(failing: &'static [u8]) -> impl Fn(&Group) -> GroupOutcome {
    move |group| {
        if set(failing).iter().all(|core| group.cores.contains(core)) {
            GroupOutcome::Failed
        } else {
            GroupOutcome::Passed
        }
    }
}

fn shape(groups: &[Group]) -> Vec<(GroupStage, Vec<u8>, Duration, bool)> {
    groups
        .iter()
        .map(|g| (g.stage, ids(g.cores), g.duration, g.escalated))
        .collect()
}

fn run(start: Start, outcome: impl FnMut(&Group) -> GroupOutcome) -> (Vec<Group>, End) {
    let mut plan = Plan::new(start).unwrap();
    let groups = drive(&mut plan, outcome);
    (groups, plan.end().unwrap())
}

#[test]
fn a_plan_needs_candidates() {
    assert_eq!(Plan::new(start(&[])).unwrap_err(), PlanError::NoCandidates);
}

#[test]
fn parked_profile_raises_the_passed_full_cycle_profile() {
    let topology = topology(&[0, 1, 2, 3]);
    let cases = [
        (
            [-10, -10, -10, -10],
            [-12, -10, -10, -10],
            [-10, -10, -10, -10],
            Some(7),
            vec![0],
        ),
        (
            [-10, -10, -10, -10],
            [-12, -8, -10, -10],
            [-10, -8, -10, -10],
            Some(7),
            vec![0],
        ),
        (
            [-12, -12, -12, -12],
            [-10, -10, -10, -10],
            [0, 0, 0, 0],
            None,
            vec![0, 1, 2, 3],
        ),
    ];
    for (passed, failing, parked, source, candidates) in cases {
        let passed = profile(&topology, &passed);
        let got = park(&profile(&topology, &failing), [(seq(7), &passed)]);
        assert_eq!(got.profile, profile(&topology, &parked));
        assert_eq!(got.source, source.map(seq));
        assert_eq!(ids(got.candidates), candidates);
    }
}

#[test]
fn parked_profile_is_all_zero_without_a_passed_profile() {
    let topology = topology(&[0, 1, 2, 3]);
    let got = park(&profile(&topology, &[-30, -30, -30, -30]), []);
    assert_eq!(got.profile, Profile::zero(&topology));
    assert_eq!(got.source, None);
    assert_eq!(ids(got.candidates), [0, 1, 2, 3]);
}

#[test]
fn parked_profile_skips_the_exact_passed_profile_for_an_older_one() {
    let topology = topology(&[0, 1]);
    let failing = profile(&topology, &[-30, -30]);
    let newest = profile(&topology, &[-30, -30]);
    let deeper = profile(&topology, &[-35, -40]);
    let older = profile(&topology, &[-20, -20]);
    let got = park(
        &failing,
        [(seq(9), &newest), (seq(8), &deeper), (seq(2), &older)],
    );
    assert_eq!(got.profile, older);
    assert_eq!(got.source, Some(seq(2)));
    assert_eq!(ids(got.candidates), [0, 1]);
}

#[test]
fn parked_profile_never_deepens_a_core_and_falls_back_to_zero_when_nothing_is_shallower() {
    let topology = topology(&[0, 1, 2]);
    let failing = profile(&topology, &[-20, -5, 0]);
    let deeper = profile(&topology, &[-40, -10, -3]);
    let got = park(&failing, [(seq(1), &deeper)]);
    assert_eq!(got.profile, Profile::zero(&topology));
    assert_eq!(got.source, None);
    assert_eq!(ids(got.candidates), [0, 1]);
    let shallower = profile(&topology, &[-10, -2, -3]);
    let got = park(&failing, [(seq(1), &shallower)]);
    assert_eq!(got.profile, profile(&topology, &[-10, -2, 0]));
    assert_eq!(got.source, Some(seq(1)));
    assert_eq!(ids(got.candidates), [0, 1]);
}

#[test]
fn located_parking_keeps_loaded_cores_and_zeroes_the_rest() {
    let topology = topology(&[0, 1, 2, 3]);
    let failing = profile(&topology, &[-30, -30, -30, 0]);
    let got = park_located(&failing, set(&[0, 1]));
    assert_eq!(got.profile, profile(&topology, &[-30, -30, 0, 0]));
    assert_eq!(got.source, None);
    assert_eq!(ids(got.candidates), [2]);
}

#[test]
fn a_group_runs_its_cores_at_failing_and_the_rest_parked() {
    let topology = topology(&[0, 1, 2, 3]);
    let failing = profile(&topology, &[-30, -31, -32, -33]);
    let parked = profile(&topology, &[-10, -10, 0, 0]);
    assert_eq!(
        group_profile(&parked, &failing, set(&[1, 3])),
        profile(&topology, &[-10, -31, 0, -33])
    );
}

#[test]
fn the_loaded_and_unloaded_candidates_split_the_first_binary_round() {
    let candidates = set(&[0, 1, 2, 3]);
    let parts = partition(candidates, 2, Some(set(&[0, 2])), CoreSet::default());
    assert_eq!(parts, [set(&[0, 2]), set(&[1, 3])]);
    assert_eq!(
        partition(candidates, 2, Some(candidates), CoreSet::default()),
        [set(&[0, 1]), set(&[2, 3])]
    );
    assert_eq!(
        partition(candidates, 2, Some(CoreSet::default()), CoreSet::default()),
        [set(&[0, 1]), set(&[2, 3])]
    );
    assert_eq!(
        partition(candidates, 4, Some(set(&[0, 2])), CoreSet::default()).len(),
        4
    );
    assert_eq!(
        partition(candidates, 2, None, CoreSet::default()),
        [set(&[0, 1]), set(&[2, 3])]
    );
}

#[test]
fn the_split_group_order_comes_from_the_plan_only_while_the_set_is_the_original() {
    let mut plan = Plan::new(Start {
        loaded: set(&[0, 2]),
        ..start(&[0, 1, 2, 3])
    })
    .unwrap();
    let groups = drive(&mut plan, passing);
    let first_round: Vec<Vec<u8>> = groups[..2].iter().map(|g| ids(g.cores)).collect();
    assert_eq!(first_round, [vec![0, 2], vec![1, 3]]);
    let parts: Vec<Vec<u8>> = groups[2..6].iter().map(|g| ids(g.cores)).collect();
    assert_eq!(parts, [vec![0], vec![1], vec![2], vec![3]]);
}

#[test]
fn partitions_are_contiguous_with_the_remainder_on_the_earlier_pieces() {
    let seven = set(&[0, 1, 2, 3, 4, 5, 6]);
    let parts = partition(seven, 3, None, CoreSet::default());
    assert_eq!(parts, [set(&[0, 1, 2]), set(&[3, 4]), set(&[5, 6])]);
    assert_eq!(partition(seven, 0, None, CoreSet::default()), [seven]);
    assert_eq!(partition(seven, 9, None, CoreSet::default()).len(), 7);
    assert!(partition(CoreSet::default(), 2, None, CoreSet::default()).is_empty());
}

#[test]
fn pieces_with_a_recent_core_go_first_and_keep_their_order_otherwise() {
    let four = set(&[0, 1, 2, 3]);
    assert_eq!(
        partition(four, 4, None, set(&[2])),
        [set(&[2]), set(&[0]), set(&[1]), set(&[3])]
    );
    assert_eq!(
        partition(four, 4, None, set(&[1, 3])),
        [set(&[1]), set(&[3]), set(&[0]), set(&[2])]
    );
    assert_eq!(
        partition(four, 2, None, set(&[3])),
        [set(&[2, 3]), set(&[0, 1])]
    );
}

#[test]
fn the_hunt_keeps_the_part_order_fixed_at_its_start() {
    let plan_start = Start {
        recent: set(&[3]),
        ..start(&[0, 1, 2, 3])
    };
    let (groups, end) = run(plan_start, passing);
    let cores: Vec<Vec<u8>> = groups.iter().map(|g| ids(g.cores)).collect();
    assert_eq!(
        cores,
        [
            vec![2, 3],
            vec![0, 1],
            vec![3],
            vec![0],
            vec![1],
            vec![2],
            vec![0, 1, 2],
            vec![1, 2, 3],
            vec![0, 2, 3],
            vec![0, 1, 3],
        ]
    );
    assert_eq!(end, End::Fallback(set(&[0, 1, 2, 3])));
    for (prefix, expected) in groups.iter().enumerate() {
        let mut plan = Plan::new(plan_start).unwrap();
        for _ in 0..prefix {
            plan.advance(GroupOutcome::Passed);
        }
        assert_eq!(plan.next_group(), Some(*expected), "prefix {prefix}");
    }
}

#[test]
fn passing_everything_visits_parts_then_complements_as_granularity_doubles() {
    let (groups, end) = run(start(&[0, 1, 2, 3]), passing);
    assert_eq!(groups.len(), 10);
    let granularity: Vec<(GroupStage, usize, usize)> = groups
        .iter()
        .map(|g| (g.stage, g.granularity, g.index))
        .collect();
    assert_eq!(
        granularity,
        [
            (GroupStage::Part, 2, 0),
            (GroupStage::Part, 2, 1),
            (GroupStage::Part, 4, 0),
            (GroupStage::Part, 4, 1),
            (GroupStage::Part, 4, 2),
            (GroupStage::Part, 4, 3),
            (GroupStage::Complement, 4, 0),
            (GroupStage::Complement, 4, 1),
            (GroupStage::Complement, 4, 2),
            (GroupStage::Complement, 4, 3),
        ]
    );
    assert_eq!(end, End::Fallback(set(&[0, 1, 2, 3])));
    assert_eq!(end.outcome(), HuntOutcome::Fallback);

    let (groups, end) = run(start(&[0, 1, 2]), passing);
    assert_eq!(groups.len(), 8);
    assert_eq!(groups[2].granularity, 3);
    assert_eq!(end, End::Fallback(set(&[0, 1, 2])));
}

#[test]
fn a_failed_part_becomes_the_set_at_granularity_two() {
    let (groups, end) = run(start(&[0, 1, 2, 3]), fails_when_holding(&[2]));
    let cores: Vec<Vec<u8>> = groups.iter().map(|g| ids(g.cores)).collect();
    assert_eq!(cores, [vec![0, 1], vec![2, 3], vec![2]]);
    assert_eq!(end, End::Culprit(core(2)));
    assert_eq!(end.outcome(), HuntOutcome::Culprit);
    assert!(groups[2].any_failed);
    assert_eq!(groups[2].set, set(&[2, 3]));
    assert_eq!(groups[2].granularity, 2);
}

#[test]
fn a_failed_complement_steps_granularity_down_to_no_less_than_two() {
    let (groups, end) = run(start(&[0, 1, 2, 3]), fails_when_holding(&[1, 2]));
    let cores: Vec<Vec<u8>> = groups.iter().map(|g| ids(g.cores)).collect();
    assert_eq!(
        cores,
        [
            vec![0, 1],
            vec![2, 3],
            vec![0],
            vec![1],
            vec![2],
            vec![3],
            vec![1, 2, 3],
            vec![1],
            vec![2],
            vec![3],
            vec![2, 3],
            vec![1, 3],
            vec![1, 2],
            vec![1],
            vec![2],
        ]
    );
    assert_eq!(end, End::Combination(set(&[1, 2])));
    assert_eq!(end.outcome(), HuntOutcome::Combination);
    assert_eq!(groups[7].granularity, 3);
    assert_eq!(groups[13].granularity, 2);
    assert_eq!(groups[13].stage, GroupStage::Part);
}

#[test]
fn a_single_candidate_is_the_culprit_without_a_group() {
    let plan = Plan::new(start(&[5])).unwrap();
    assert_eq!(plan.next_group(), None);
    assert_eq!(plan.end(), Some(End::Culprit(core(5))));
}

#[test]
fn a_skipped_group_advances_like_a_passed_one() {
    let (passed, _) = run(start(&[0, 1, 2, 3]), passing);
    let (skipped, end) = run(start(&[0, 1, 2, 3]), |_| GroupOutcome::Skipped);
    assert_eq!(passed, skipped);
    assert_eq!(end, End::Fallback(set(&[0, 1, 2, 3])));
}

#[test]
fn the_duration_escalates_only_for_a_longer_failure() {
    let short = (GroupStage::Part, Duration::Short, false);
    let expected_short = [short, short];
    for longer in [false, true] {
        let (groups, end) = run(
            Start {
                longer_failure: longer,
                ..start(&[0, 1])
            },
            passing,
        );
        let got: Vec<_> = groups
            .iter()
            .map(|g| (g.stage, g.duration, g.escalated))
            .collect();
        if longer {
            assert_eq!(
                got,
                [
                    short,
                    short,
                    (GroupStage::Full, Duration::Short, false),
                    (GroupStage::Part, Duration::Failed, true),
                    (GroupStage::Part, Duration::Failed, true),
                ]
            );
        } else {
            assert_eq!(got, expected_short);
        }
        assert_eq!(end, End::Fallback(set(&[0, 1])));
    }
}

#[test]
fn the_full_group_runs_the_whole_set_once() {
    let (groups, _) = run(
        Start {
            longer_failure: true,
            ..start(&[0, 1])
        },
        passing,
    );
    let full = &groups[2];
    assert_eq!(full.stage, GroupStage::Full);
    assert_eq!(full.cores, set(&[0, 1]));
    assert_eq!(full.set, set(&[0, 1]));
    let after = &groups[3];
    assert!(after.escalated);
    assert!(!after.full_checked);
    assert!(!after.any_failed);
    assert_eq!(after.set, set(&[0, 1]));
    assert_eq!(after.granularity, 2);
    assert_eq!(after.index, 0);
}

#[test]
fn a_failed_full_group_makes_the_candidates_a_combination() {
    let (groups, end) = run(
        Start {
            longer_failure: true,
            ..start(&[0, 1])
        },
        |group| {
            if group.stage == GroupStage::Full {
                GroupOutcome::Failed
            } else {
                GroupOutcome::Passed
            }
        },
    );
    assert_eq!(
        groups
            .iter()
            .filter(|g| g.stage == GroupStage::Full)
            .count(),
        1
    );
    assert!(groups.iter().all(|g| !g.escalated));
    assert_eq!(end, End::Combination(set(&[0, 1])));
}

#[test]
fn a_skipped_full_group_neither_escalates_nor_fails() {
    let (groups, end) = run(
        Start {
            longer_failure: true,
            ..start(&[0, 1, 2])
        },
        |group| {
            if group.stage == GroupStage::Full {
                GroupOutcome::Skipped
            } else {
                GroupOutcome::Passed
            }
        },
    );
    assert!(
        groups
            .iter()
            .all(|g| !g.escalated && g.duration == Duration::Short)
    );
    assert_eq!(
        groups
            .iter()
            .filter(|g| g.stage == GroupStage::Full)
            .count(),
        1
    );
    assert_eq!(end, End::Fallback(set(&[0, 1, 2])));
}

#[test]
fn a_failed_full_group_goes_on_to_finer_granularity() {
    let (groups, end) = run(
        Start {
            longer_failure: true,
            ..start(&[0, 1, 2, 3])
        },
        |group| {
            if group.stage == GroupStage::Full {
                GroupOutcome::Failed
            } else {
                GroupOutcome::Passed
            }
        },
    );
    let full = groups
        .iter()
        .position(|g| g.stage == GroupStage::Full)
        .unwrap();
    assert_eq!(groups[full + 1].granularity, 4);
    assert!(groups[full + 1].any_failed);
    assert!(groups[full + 1].full_checked);
    assert_eq!(end, End::Combination(set(&[0, 1, 2, 3])));
}

#[test]
fn a_narrowed_set_never_runs_the_full_check() {
    let (groups, end) = run(
        Start {
            longer_failure: true,
            ..start(&[0, 1, 2, 3])
        },
        fails_when_holding(&[2]),
    );
    assert!(groups.iter().all(|g| g.stage != GroupStage::Full));
    assert_eq!(end, End::Culprit(core(2)));
}

#[test]
fn the_duration_prior_starts_at_the_failed_duration_without_a_full_check() {
    let (groups, end) = run(
        Start {
            longer_failure: true,
            duration_prior: true,
            ..start(&[0, 1])
        },
        passing,
    );
    assert_eq!(groups.len(), 2);
    assert!(
        groups
            .iter()
            .all(|g| g.duration == Duration::Failed && g.escalated && g.stage == GroupStage::Part)
    );
    assert_eq!(end, End::Fallback(set(&[0, 1])));
}

#[test]
fn the_repeated_core_probe_runs_alone_first_and_returns_to_binary_parts_after_a_pass() {
    let plan_start = Start {
        recent: set(&[3]),
        repeated_core: Some(core(3)),
        ..start(&[0, 1, 2, 3])
    };
    let mut plan = Plan::new(plan_start).unwrap();
    let probe = plan.next_group().unwrap();
    assert_eq!(probe.stage, GroupStage::Repeated);
    assert_eq!(probe.cores, set(&[3]));
    assert_eq!(probe.granularity, 4);
    assert_eq!(probe.index, 0);
    plan.advance(GroupOutcome::Passed);
    let next = plan.next_group().unwrap();
    assert_eq!(next.stage, GroupStage::Part);
    assert_eq!((next.granularity, next.index), (2, 0));
    assert_eq!(next.cores, set(&[2, 3]));
    assert!(!next.any_failed);

    let mut skipped = Plan::new(plan_start).unwrap();
    skipped.advance(GroupOutcome::Skipped);
    assert_eq!(skipped.next_group(), Some(next));
}

#[test]
fn a_failed_repeated_core_probe_is_the_culprit() {
    let mut plan = Plan::new(Start {
        repeated_core: Some(core(1)),
        ..start(&[0, 1, 2])
    })
    .unwrap();
    assert_eq!(plan.next_group().unwrap().cores, set(&[1]));
    plan.advance(GroupOutcome::Failed);
    assert_eq!(plan.next_group(), None);
    assert_eq!(plan.end(), Some(End::Culprit(core(1))));
}

#[test]
fn the_repeated_core_probe_needs_more_than_two_candidates_and_a_candidate() {
    let two = Plan::new(Start {
        repeated_core: Some(core(1)),
        ..start(&[0, 1])
    })
    .unwrap();
    assert_eq!(two.next_group().unwrap().stage, GroupStage::Part);
    let outsider = Plan::new(Start {
        repeated_core: Some(core(9)),
        ..start(&[0, 1, 2])
    })
    .unwrap();
    assert_eq!(outsider.next_group().unwrap().stage, GroupStage::Part);
}

#[test]
fn a_located_hunt_runs_the_empty_locate_group_first_at_the_failed_duration() {
    let plan = Plan::new(located(&[2, 3], &[0, 1], false)).unwrap();
    let locate = plan.next_group().unwrap();
    assert_eq!(locate.stage, GroupStage::Locate);
    assert!(locate.cores.is_empty());
    assert_eq!(locate.duration, Duration::Failed);
    assert_eq!(locate.set, set(&[2, 3]));
}

#[test]
fn a_failed_or_skipped_locate_ends_the_hunt_loaded() {
    for outcome in [GroupOutcome::Failed, GroupOutcome::Skipped] {
        let mut plan = Plan::new(located(&[2, 3], &[0, 1], false)).unwrap();
        plan.advance(outcome);
        assert_eq!(plan.next_group(), None);
        assert_eq!(plan.end(), Some(End::Loaded));
        assert_eq!(plan.end().unwrap().outcome(), HuntOutcome::Loaded);
    }
}

#[test]
fn a_located_hunt_retests_the_full_failing_profile_before_ending_loaded() {
    let (groups, end) = run(located(&[2, 3], &[0, 1], false), passing);
    assert_eq!(
        shape(&groups),
        [
            (GroupStage::Locate, vec![], Duration::Failed, false),
            (GroupStage::Part, vec![2], Duration::Short, false),
            (GroupStage::Part, vec![3], Duration::Short, false),
            (GroupStage::Full, vec![2, 3], Duration::Failed, false),
        ]
    );
    assert_eq!(end, End::Loaded);
}

#[test]
fn a_located_hunt_with_loaded_cores_at_zero_ends_fallback_after_the_full_group() {
    let (groups, end) = run(located(&[2, 3], &[0, 1], true), passing);
    assert_eq!(groups.len(), 4);
    assert_eq!(end, End::Fallback(set(&[2, 3])));
}

#[test]
fn a_skipped_located_full_group_ends_like_a_passed_one() {
    let (_, end) = run(located(&[2, 3], &[0, 1], false), |group| {
        if group.stage == GroupStage::Full {
            GroupOutcome::Skipped
        } else {
            GroupOutcome::Passed
        }
    });
    assert_eq!(end, End::Loaded);
}

#[test]
fn a_failed_located_full_group_makes_the_candidates_a_combination() {
    let (groups, end) = run(located(&[2, 3], &[0, 1], false), |group| {
        if group.stage == GroupStage::Full {
            GroupOutcome::Failed
        } else {
            GroupOutcome::Passed
        }
    });
    assert_eq!(groups.len(), 4);
    assert_eq!(end, End::Combination(set(&[2, 3])));
}

#[test]
fn an_escalated_located_hunt_retests_the_full_profile_at_the_failed_duration() {
    let plan_start = Start {
        longer_failure: true,
        ..located(&[2, 3], &[0, 1], false)
    };
    let (groups, end) = run(plan_start, passing);
    let part = |duration, escalated, core| (GroupStage::Part, vec![core], duration, escalated);
    assert_eq!(
        shape(&groups),
        [
            (GroupStage::Locate, vec![], Duration::Failed, false),
            part(Duration::Short, false, 2),
            part(Duration::Short, false, 3),
            (GroupStage::Full, vec![2, 3], Duration::Short, false),
            part(Duration::Failed, true, 2),
            part(Duration::Failed, true, 3),
            (GroupStage::Full, vec![2, 3], Duration::Failed, true),
        ]
    );
    assert_eq!(end, End::Loaded);

    let (_, end) = run(plan_start, |group| {
        if group.stage == GroupStage::Full && group.duration == Duration::Failed {
            GroupOutcome::Failed
        } else {
            GroupOutcome::Passed
        }
    });
    assert_eq!(end, End::Combination(set(&[2, 3])));
}

#[test]
fn a_located_hunt_narrows_unloaded_candidates_like_any_other() {
    let (groups, end) = run(
        located(&[2, 3, 4, 5], &[0, 1], false),
        fails_when_holding(&[4]),
    );
    assert_eq!(groups[0].stage, GroupStage::Locate);
    assert_eq!(end, End::Culprit(core(4)));
    assert!(groups.iter().skip(1).all(|g| g.duration == Duration::Short));
}

#[test]
fn a_located_hunt_with_one_candidate_names_it_after_the_locate_passes() {
    let mut plan = Plan::new(located(&[4], &[0, 1], false)).unwrap();
    assert_eq!(plan.next_group().unwrap().stage, GroupStage::Locate);
    plan.advance(GroupOutcome::Passed);
    assert_eq!(plan.end(), Some(End::Culprit(core(4))));
}

#[test]
fn a_located_hunt_can_start_at_the_failed_duration_and_still_checks_the_full_profile() {
    let (groups, end) = run(
        Start {
            longer_failure: true,
            duration_prior: true,
            ..located(&[2, 3], &[0, 1], false)
        },
        passing,
    );
    assert!(
        groups
            .iter()
            .skip(1)
            .all(|g| g.duration == Duration::Failed && g.escalated)
    );
    assert_eq!(groups.last().unwrap().stage, GroupStage::Full);
    assert_eq!(end, End::Loaded);
}

#[test]
fn a_finished_plan_ignores_further_outcomes() {
    let mut plan = Plan::new(start(&[0, 1])).unwrap();
    drive(&mut plan, fails_when_holding(&[0]));
    let ended = plan.clone();
    plan.advance(GroupOutcome::Failed);
    assert_eq!(plan, ended);
    assert_eq!(plan.set(), set(&[0]));
}

#[test]
fn the_plan_keeps_the_duration_of_its_last_group_for_member_probes() {
    let mut plan = Plan::new(Start {
        longer_failure: true,
        ..start(&[0, 1])
    })
    .unwrap();
    assert_eq!(plan.duration(), Duration::Short);
    drive(&mut plan, passing);
    assert_eq!(plan.duration(), Duration::Failed);
    assert_eq!(plan.set(), set(&[0, 1]));
}

fn two_cores() -> (crate::Topology, Profile, Profile) {
    let topology = topology(&[0, 1]);
    let failing = profile(&topology, &[-30, -30]);
    let parked = profile(&topology, &[0, 0]);
    (topology, failing, parked)
}

fn probe_until_done(
    members: &[u8],
    failing: &Profile,
    parked: &Profile,
    fails: impl Fn(&Profile) -> bool,
) -> (Vec<Probe>, Vec<ProbeResult>, Found) {
    let probes = MemberProbes::new(set(members), failing, parked);
    let mut results = Vec::new();
    let mut ran = Vec::new();
    loop {
        match probes.step(&results) {
            ProbeStep::Done(found) => return (ran, results, found),
            ProbeStep::Run(probe) => {
                assert!(ran.len() < 200, "probing did not terminate");
                assert_eq!(probe.index, results.len());
                let outcome = if fails(&probe.profile(parked)) {
                    GroupOutcome::Failed
                } else {
                    GroupOutcome::Passed
                };
                results.push(ProbeResult {
                    member: probe.member,
                    offset: probe.offset,
                    outcome,
                });
                ran.push(probe);
            }
        }
    }
}

#[test]
fn member_probes_gallop_then_bisect_one_member_at_a_time() {
    let (_, failing, parked) = two_cores();
    let (ran, _, found) = probe_until_done(&[0, 1], &failing, &parked, |p| {
        p.get(core(0)).unwrap().get() <= -30 && p.get(core(1)).unwrap().get() <= -20
    });
    let order: Vec<(u8, i64)> = ran
        .iter()
        .map(|probe| (probe.member.get(), probe.offset.get()))
        .collect();
    assert_eq!(
        order,
        [
            (0, -29),
            (1, -29),
            (1, -28),
            (1, -26),
            (1, -22),
            (1, -14),
            (1, -18),
            (1, -20),
            (1, -19),
        ]
    );
    assert!(!found.stopped);
    assert_eq!(found.members.get(core(0)), Some(off(-30)));
    assert_eq!(found.members.get(core(1)), Some(off(-20)));
    for probe in &ran[1..] {
        assert_eq!(probe.held.get(core(0)), Some(off(-30)));
        assert_eq!(probe.held.get(core(1)), None);
    }
    assert_eq!(ran[0].held.get(core(1)), Some(off(-30)));
}

#[test]
fn already_probed_members_are_held_at_their_shallowest_failing_offset() {
    let (_, failing, parked) = two_cores();
    let (ran, _, found) = probe_until_done(&[0, 1], &failing, &parked, |p| {
        p.get(core(0)).unwrap().get() <= -28 && p.get(core(1)).unwrap().get() <= -30
    });
    assert_eq!(found.members.get(core(0)), Some(off(-28)));
    let first_of_second = ran.iter().find(|probe| probe.member == core(1)).unwrap();
    assert_eq!(first_of_second.held.get(core(0)), Some(off(-28)));
    assert!(!first_of_second.holds_failing(&failing));
    assert_eq!(
        first_of_second.profile(&parked),
        profile(&topology(&[0, 1]), &[-28, first_of_second.offset.get()])
    );
}

#[test]
fn a_skipped_probe_stops_probing_with_the_failures_known_so_far() {
    let (_, failing, parked) = two_cores();
    let probes = MemberProbes::new(set(&[0, 1]), &failing, &parked);
    let results = [
        ProbeResult {
            member: core(0),
            offset: off(-29),
            outcome: GroupOutcome::Failed,
        },
        ProbeResult {
            member: core(0),
            offset: off(-28),
            outcome: GroupOutcome::Skipped,
        },
    ];
    let ProbeStep::Done(found) = probes.step(&results) else {
        panic!("probing continued past a skipped probe");
    };
    assert!(found.stopped);
    assert_eq!(found.members.get(core(0)), Some(off(-29)));
    assert_eq!(found.members.get(core(1)), Some(off(-30)));
}

#[test]
fn a_probe_that_passed_can_become_a_failure() {
    let (_, failing, parked) = two_cores();
    let probes = MemberProbes::new(set(&[0, 1]), &failing, &parked);
    let mut results = vec![ProbeResult {
        member: core(0),
        offset: off(-29),
        outcome: GroupOutcome::Passed,
    }];
    let ProbeStep::Run(next) = probes.step(&results) else {
        panic!("member 0 is one count wide, so member 1 is next");
    };
    assert_eq!((next.member, next.offset), (core(1), off(-29)));
    results[0].outcome = GroupOutcome::Failed;
    let ProbeStep::Run(next) = probes.step(&results) else {
        panic!("member 0 has a failure at -29 and room above it");
    };
    assert_eq!((next.member, next.offset), (core(0), off(-28)));
    assert_eq!(next.index, 1);
}

#[test]
fn a_member_one_count_below_its_parked_offset_needs_no_probe() {
    let topology = topology(&[0, 1]);
    let failing = profile(&topology, &[-30, -30]);
    let parked = profile(&topology, &[-29, 0]);
    let probes = MemberProbes::new(set(&[0, 1]), &failing, &parked);
    let ProbeStep::Run(probe) = probes.step(&[]) else {
        panic!("member 1 still needs probing");
    };
    assert_eq!(probe.member, core(1));
}

#[test]
fn the_gallop_shift_saturates_instead_of_overflowing() {
    let (_, failing, parked) = two_cores();
    let probes = MemberProbes::new(set(&[0]), &failing, &parked);
    let results: Vec<ProbeResult> = (0..70)
        .map(|_| ProbeResult {
            member: core(0),
            offset: off(-29),
            outcome: GroupOutcome::Failed,
        })
        .collect();
    let ProbeStep::Run(probe) = probes.step(&results) else {
        panic!("member 0 still has room");
    };
    assert_eq!(probe.offset, off(-15));
}

#[test]
fn a_combination_is_already_broken_when_a_member_is_shallower_in_the_profile() {
    let topology = topology(&[0, 1]);
    let members: PerCore<TargetOffset> = [(core(0), off(-30)), (core(1), off(-30))]
        .into_iter()
        .collect();
    assert!(already_broken(&members, &profile(&topology, &[-29, -30])));
    assert!(!already_broken(&members, &profile(&topology, &[-30, -30])));
    assert!(!already_broken(&members, &profile(&topology, &[-35, -40])));
}

fn combination(topology: &crate::Topology, pairs: &[(u8, i64)]) -> Constraints {
    let mut constraints = Constraints::new(topology);
    constraints
        .insert_combination(
            CombinationId::new(NonZeroU32::new(1).unwrap()),
            pairs.iter().map(|&(id, v)| (core(id), off(v))).collect(),
        )
        .unwrap();
    constraints
}

fn members(pairs: &[(u8, i64)]) -> PerCore<TargetOffset> {
    pairs.iter().map(|&(id, v)| (core(id), off(v))).collect()
}

fn passed_probe(member: u8, offset: i64, held: &[(u8, i64)], index: usize) -> Probe {
    Probe {
        member: core(member),
        offset: off(offset),
        held: members(held),
        index,
    }
}

fn ranked(topology: &crate::Topology, order: &[u8]) -> Ranking {
    Ranking::new(topology, order.iter().map(|&id| core(id)).collect()).unwrap()
}

#[test]
fn the_backoff_takes_the_tested_probe_that_moves_its_member_the_fewest_counts() {
    let topology = topology(&[0, 1]);
    let constraints = combination(&topology, &[(0, -26), (1, -30)]);
    let failing = profile(&topology, &[-30, -30]);
    let current = profile(&topology, &[-30, -30]);
    let ranking = ranked(&topology, &[0, 1]);
    let combination_members = members(&[(0, -26), (1, -30)]);
    let far = passed_probe(0, -25, &[(1, -30)], 3);
    let near = passed_probe(1, -29, &[(0, -30)], 4);
    assert_eq!(
        combination_backoff(
            &combination_members,
            &[far],
            &failing,
            &current,
            &constraints,
            &ranking
        ),
        Some(Backoff::Tested {
            probe: 0,
            member: core(0),
            to: off(-25),
        })
    );
    assert_eq!(
        combination_backoff(
            &combination_members,
            &[far, near],
            &failing,
            &current,
            &constraints,
            &ranking
        ),
        Some(Backoff::Tested {
            probe: 1,
            member: core(1),
            to: off(-29),
        })
    );
}

#[test]
fn equal_moves_go_to_the_lowest_ranked_member() {
    let topology = topology(&[0, 1]);
    let constraints = combination(&topology, &[(0, -30), (1, -30)]);
    let failing = profile(&topology, &[-30, -30]);
    let current = failing;
    let combination_members = members(&[(0, -30), (1, -30)]);
    let probes = [
        passed_probe(0, -29, &[(1, -30)], 0),
        passed_probe(1, -29, &[(0, -30)], 1),
    ];
    let pick = |order: &[u8]| {
        combination_backoff(
            &combination_members,
            &probes,
            &failing,
            &current,
            &constraints,
            &ranked(&topology, order),
        )
    };
    assert_eq!(
        pick(&[0, 1]),
        Some(Backoff::Tested {
            probe: 1,
            member: core(1),
            to: off(-29),
        })
    );
    assert_eq!(
        pick(&[1, 0]),
        Some(Backoff::Tested {
            probe: 0,
            member: core(0),
            to: off(-29),
        })
    );
}

#[test]
fn probes_that_moved_other_members_or_do_not_help_are_not_tested_moves() {
    let topology = topology(&[0, 1]);
    let constraints = combination(&topology, &[(0, -26), (1, -30)]);
    let failing = profile(&topology, &[-30, -30]);
    let current = profile(&topology, &[-30, -30]);
    let ranking = ranked(&topology, &[0, 1]);
    let combination_members = members(&[(0, -26), (1, -30)]);
    let untested = Some(Backoff::Untested {
        member: core(1),
        to: off(-29),
        reach: 79,
    });
    let held_off_failing = passed_probe(1, -29, &[(0, -26)], 0);
    let still_reaching = passed_probe(0, -27, &[(1, -30)], 1);
    assert_eq!(
        combination_backoff(
            &combination_members,
            &[held_off_failing, still_reaching],
            &failing,
            &current,
            &constraints,
            &ranking
        ),
        untested
    );
    let no_move = passed_probe(0, -30, &[(1, -30)], 0);
    assert_eq!(
        combination_backoff(
            &combination_members,
            &[no_move],
            &failing,
            &current,
            &constraints,
            &ranking
        ),
        untested
    );
    let shallower_now = profile(&topology, &[-28, -30]);
    let behind = passed_probe(0, -29, &[(1, -30)], 0);
    assert_eq!(
        combination_backoff(
            &combination_members,
            &[behind],
            &failing,
            &shallower_now,
            &constraints,
            &ranking
        )
        .map(|b| matches!(b, Backoff::Untested { .. })),
        Some(true)
    );
}

#[test]
fn without_a_tested_move_the_member_leaving_the_most_depth_moves_one_count_past() {
    let topology = topology(&[0, 1]);
    let constraints = combination(&topology, &[(0, -30), (1, -20)]);
    let current = profile(&topology, &[-30, -30]);
    let ranking = ranked(&topology, &[0, 1]);
    assert_eq!(
        combination_backoff(
            &members(&[(0, -30), (1, -20)]),
            &[],
            &current,
            &current,
            &constraints,
            &ranking
        ),
        Some(Backoff::Untested {
            member: core(0),
            to: off(-29),
            reach: 79,
        })
    );
}

#[test]
fn equal_reach_goes_to_the_lowest_ranked_member() {
    let topology = topology(&[0, 1]);
    let constraints = combination(&topology, &[(0, -30), (1, -30)]);
    let current = profile(&topology, &[-30, -30]);
    let pick = |order: &[u8]| {
        combination_backoff(
            &members(&[(0, -30), (1, -30)]),
            &[],
            &current,
            &current,
            &constraints,
            &ranked(&topology, order),
        )
    };
    assert_eq!(
        pick(&[0, 1]),
        Some(Backoff::Untested {
            member: core(1),
            to: off(-29),
            reach: 79,
        })
    );
    assert_eq!(
        pick(&[1, 0]),
        Some(Backoff::Untested {
            member: core(0),
            to: off(-29),
            reach: 79,
        })
    );
}

#[test]
fn no_backoff_when_no_member_can_leave_a_safe_profile() {
    let topology = topology(&[0, 1]);
    let mut constraints = combination(&topology, &[(0, -30), (1, -30)]);
    constraints
        .insert_failure_point(core(0), FailurePoint::new(off(0)))
        .unwrap();
    let current = profile(&topology, &[-30, -30]);
    assert_eq!(
        combination_backoff(
            &members(&[(0, -30), (1, -30)]),
            &[],
            &current,
            &current,
            &constraints,
            &ranked(&topology, &[0, 1])
        ),
        None
    );
}

#[derive(Debug)]
struct Hunt {
    candidates: Vec<u8>,
    loaded_mask: u16,
    recent_mask: u16,
    failing: Option<u16>,
    longer: bool,
    prior: bool,
    located: Option<bool>,
    repeated: Option<usize>,
}

fn hunts() -> impl Strategy<Value = Hunt> {
    let cores: Vec<u8> = (0..16).collect();
    (
        prop::sample::subsequence(cores, 2..=8),
        any::<u16>(),
        any::<u16>(),
        prop::option::weighted(0.85, 1..=u16::MAX),
        any::<bool>(),
        any::<bool>(),
        prop::option::of(any::<bool>()),
        prop::option::of(0_usize..8),
    )
        .prop_map(
            |(candidates, loaded_mask, recent_mask, failing, longer, prior, located, repeated)| {
                Hunt {
                    candidates,
                    loaded_mask,
                    recent_mask,
                    failing,
                    longer,
                    prior,
                    located,
                    repeated,
                }
            },
        )
}

fn mask(ids: &[u8], bits: u16) -> CoreSet {
    ids.iter()
        .filter(|&&id| bits >> id & 1 == 1)
        .map(|&id| core(id))
        .collect()
}

proptest! {
    #![proptest_config(crate::prop_tests::seeded(256))]

    #[test]
    fn plans_terminate_on_the_minimal_failing_set_or_fall_back(hunt in hunts()) {
        let candidates = set(&hunt.candidates);
        let failing = hunt.failing.map(|bits| mask(&hunt.candidates, bits)).filter(|set| !set.is_empty());
        let plan_start = Start {
            candidates,
            loaded: mask(&hunt.candidates, hunt.loaded_mask),
            recent: mask(&hunt.candidates, hunt.recent_mask),
            longer_failure: hunt.longer,
            duration_prior: hunt.prior,
            located: hunt.located.map(|loaded_at_zero| Located { loaded_at_zero }),
            repeated_core: hunt.repeated.and_then(|i| hunt.candidates.get(i)).map(|&id| core(id)),
        };
        let mut plan = Plan::new(plan_start).unwrap();
        let mut seen = HashSet::new();
        let mut groups = 0;
        let bound = 8 * hunt.candidates.len() * hunt.candidates.len() + 16;
        while let Some(group) = plan.next_group() {
            groups += 1;
            prop_assert!(groups <= bound, "{groups} groups exceed {bound}");
            let key = (
                group.set,
                group.granularity,
                group.duration,
                group.stage,
                group.index,
            );
            prop_assert!(seen.insert(key), "group repeated: {group:?}");
            let fails = failing.is_some_and(|f| f.iter().all(|c| group.cores.contains(c)));
            plan.advance(if fails { GroupOutcome::Failed } else { GroupOutcome::Passed });
        }
        let end = plan.end().unwrap();
        let none_failed = |set: CoreSet| match hunt.located {
            Some(true) | None => End::Fallback(set),
            Some(false) => End::Loaded,
        };
        let expected = match failing {
            None => none_failed(candidates),
            Some(f) if f.len() == 1 => End::Culprit(f.iter().next().unwrap()),
            Some(f) if f != candidates => End::Combination(f),
            Some(f) if hunt.located.is_some() || (hunt.longer && !hunt.prior) => End::Combination(f),
            Some(f) => End::Fallback(f),
        };
        prop_assert_eq!(end, expected);
        prop_assert_eq!(plan.next_group(), None);
    }

    #[test]
    fn partitions_cover_the_set_in_order_with_even_sizes(
        bits in 1_u16..,
        granularity in 0_usize..20,
        recent in any::<u16>(),
    ) {
        let all: Vec<u8> = (0..16).collect();
        let whole = mask(&all, bits);
        let parts = partition(whole, granularity, None, mask(&all, recent));
        prop_assert_eq!(parts.len(), granularity.clamp(1, whole.len()));
        let mut covered = CoreSet::default();
        for part in &parts {
            prop_assert!(!part.is_empty());
            for core in part {
                prop_assert!(covered.insert(core));
            }
        }
        prop_assert_eq!(covered, whole);
        let sizes: Vec<usize> = parts.iter().map(CoreSet::len).collect();
        prop_assert!(sizes.iter().max().unwrap() - sizes.iter().min().unwrap() <= 1);
    }

    #[test]
    fn parked_offsets_are_never_deeper_than_failing_and_name_the_deeper_cores(
        failing in prop::collection::vec(-50_i64..=0, 4),
        passed in prop::collection::vec(prop::collection::vec(-50_i64..=0, 4), 0..4),
    ) {
        let topology = topology(&[0, 1, 2, 3]);
        let failing = profile(&topology, &failing);
        let passed: Vec<Profile> = passed.iter().map(|values| profile(&topology, values)).collect();
        let tagged: Vec<(Seq, &Profile)> = passed
            .iter()
            .enumerate()
            .map(|(i, p)| (seq(i as u64 + 1), p))
            .collect();
        let parked = park(&failing, tagged);
        for (core, offset) in failing.iter() {
            let at = parked.profile.get(core).unwrap();
            prop_assert!(!at.deeper_than(offset));
            prop_assert_eq!(parked.candidates.contains(core), offset.deeper_than(at));
        }
        if parked.source.is_none() {
            prop_assert_eq!(parked.profile, Profile::zero(&topology));
        }
    }
}
