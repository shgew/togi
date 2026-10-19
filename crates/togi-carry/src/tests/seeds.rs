use test_case::test_case;
use togi_journal::{
    Attribution, CarriedSource, CorePhase, CoreTransition, Payload, ResetTarget, SCHEMA,
    SessionCarried, Shutdown, ShutdownReason,
};
use togi_machine::{Condition, Signal};

use super::{
    Archive, DEFECTS, Source, backoff_decision, chain_of, context, culprit, day, hunt_start, limit,
    offset, other_context, pending, point, profile, reset, resolve, seed, seq, set, workload,
};
use crate::settled;

fn carried_source(id: togi_journal::SessionId, ruleset: u32) -> CarriedSource {
    CarriedSource {
        session: id,
        schema: SCHEMA,
        ruleset,
    }
}

#[test]
fn seeds_solo_limits_and_failure_points() {
    let mut x = Source::new(day(9), &[0, 1, 2, 6], 3, 1).with_context(&context());
    x.pass(0, -30, Condition::Alone);
    let solo0 = x.pass(0, -35, Condition::Alone);
    let point0 = x
        .fail(0, -36, Condition::Alone, Attribution::Attributed)
        .failure;
    let point1 = x
        .fail(1, -40, Condition::Together, Attribution::Attributed)
        .failure;
    let solo1 = x.pass(1, -40, Condition::Alone);
    x.pass(1, -45, Condition::Together);
    x.fail(2, -20, Condition::Together, Attribution::Unattributed);
    let point6 = x
        .fail(6, 0, Condition::Alone, Attribution::Attributed)
        .failure;

    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &[]);

    assert_eq!(got.sources(), [carried_source(x.id, 3)]);
    assert_eq!(got.context(), Some(&context()));
    assert_eq!(
        got.seeds(),
        [
            seed(
                0,
                Some(limit(-35, x.id, solo0)),
                Some(point(-36, x.id, point0, Signal::UnexpectedExit)),
            ),
            seed(
                1,
                Some(limit(-40, x.id, solo1)),
                Some(point(-40, x.id, point1, Signal::UnexpectedExit)),
            ),
            seed(
                6,
                None,
                Some(point(0, x.id, point6, Signal::UnexpectedExit))
            ),
        ]
    );
}

#[test]
fn sparse_seed_reset_and_reinsert_preserve_order_and_wire_shape() {
    let mut x = Source::new(day(9), &[0, 7, 15], 3, 1).with_context(&context());
    x.pass(15, -30, Condition::Alone);
    x.fail(15, -31, Condition::Alone, Attribution::Attributed);
    x.pass(7, -20, Condition::Alone);
    x.fail(7, -21, Condition::Alone, Attribution::Attributed);
    x.add(reset(ResetTarget::Core(super::core(7))));
    x.add(reset(ResetTarget::Core(super::core(15))));
    let solo = x.pass(15, -10, Condition::Alone);
    let failure = x
        .fail(15, -11, Condition::Alone, Attribution::Attributed)
        .failure;
    let low = x.pass(0, -5, Condition::Alone);

    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &[]);
    assert_eq!(
        got.seeds(),
        [
            seed(0, Some(limit(-5, x.id, low)), None),
            seed(
                15,
                Some(limit(-10, x.id, solo)),
                Some(point(-11, x.id, failure, Signal::UnexpectedExit))
            ),
        ]
    );
    assert_eq!(
        serde_json::to_value(resolve(&got, 1).session_carried(set(&[0, 7, 15])).carried).unwrap(),
        serde_json::json!([
            {
                "core": 0,
                "candidate_solo_limit": {"offset": -5, "from": {"session": x.id, "seq": low}}
            },
            {
                "core": 15,
                "candidate_solo_limit": {"offset": -10, "from": {"session": x.id, "seq": solo}},
                "failure_point": {
                    "offset": -11,
                    "from": {"session": x.id, "seq": failure},
                    "signal": "unexpected_exit"
                }
            }
        ])
    );
}

#[test_case(Some(&context()), &context(), true, None; "the same BIOS keeps the failure points")]
#[test_case(
    Some(&context()),
    &other_context(),
    false,
    Some("bios_version is 3.20; the session recorded 3.10");
    "a changed BIOS drops the failure points"
)]
#[test_case(
    None,
    &other_context(),
    false,
    Some("the archived session recorded no BIOS context");
    "a session that recorded no BIOS context"
)]
fn bios_change_archives_metadata_carry_available(
    recorded: Option<&togi_machine::BiosContext>,
    current: &togi_machine::BiosContext,
    kept: bool,
    detail: Option<&str>,
) {
    let mut x = Source::new(day(9), &[0, 1], 4, 1);
    if let Some(recorded) = recorded {
        x = x.with_context(recorded);
    }
    let failure = x
        .fail(0, -30, Condition::Alone, Attribution::Attributed)
        .failure;
    let solo = x.pass(1, -20, Condition::Alone);
    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &[]);
    let resolved = got.resolve(current, &togi_config::Backends::default(), 1);

    assert_eq!(resolved.failure_points(), kept);
    assert_eq!(resolved.detail(), detail);
    let want = if kept {
        vec![
            seed(
                0,
                None,
                Some(point(-30, x.id, failure, Signal::UnexpectedExit)),
            ),
            seed(1, Some(limit(-20, x.id, solo)), None),
        ]
    } else {
        vec![seed(1, Some(limit(-20, x.id, solo)), None)]
    };
    assert_eq!(resolved.seeds(), want);
    let carried = resolved.session_carried(set(&[0, 1]));
    assert_eq!(carried.failure_points, kept);
    assert_eq!(carried.carried, want);
}

#[test]
fn select_idempotent() {
    let mut x = Source::new(day(9), &[0], 4, 1).with_context(&context());
    x.fail(0, -30, Condition::Alone, Attribution::Attributed);
    let (mut archive, chain) = chain_of(&[&x]);
    let first = pending(&mut archive, &chain, None, &[]);
    let second = pending(&mut archive, &chain, None, &[]);
    assert_eq!(first, second);
    assert_eq!(resolve(&first, 1), resolve(&second, 1));
}

#[test_case(false, 0, Some((-35, true)); "culprit")]
#[test_case(true, 0, None; "reset culprit")]
#[test_case(false, -28, Some((-28, false)); "attributed failure is shallower")]
#[test_case(false, -40, Some((-35, true)); "culprit is shallower")]
fn hunt_culprit_seed_sparse_topology(
    reset_after: bool,
    attributed: i64,
    want: Option<(i64, bool)>,
) {
    let mut x = Source::new(day(9), &[0, 2, 5], 3, 1).with_context(&context());
    let failure = x.add(super::idle_failure(profile(&[
        (0, -10),
        (2, -35),
        (5, -20),
    ])));
    x.add(hunt_start(
        1,
        failure,
        profile(&[(0, -10), (2, -35), (5, -20)]),
        set(&[2]),
    ));
    let end = x.add(culprit(1, set(&[2])));
    if reset_after {
        x.add(reset(ResetTarget::Core(super::core(2))));
    }
    let attributed_failure = (attributed != 0).then(|| {
        x.fail(2, attributed, Condition::Together, Attribution::Attributed)
            .failure
    });

    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &[]);
    let want: Vec<_> = want
        .map(|(value, from_hunt)| {
            let (at, signal) = if from_hunt {
                (end, Signal::Crash)
            } else {
                (attributed_failure.unwrap(), Signal::UnexpectedExit)
            };
            seed(2, None, Some(point(value, x.id, at, signal)))
        })
        .into_iter()
        .collect();
    assert_eq!(got.seeds(), want);
}

fn member(core: i64, value: i64) -> togi_journal::Member {
    togi_journal::Member {
        core: super::core(core),
        offset: offset(value),
    }
}

#[test_case(vec![member(2, -23)], -23; "refined member")]
#[test_case(Vec::new(), -30; "no members from an older end")]
#[test_case(vec![member(5, -23)], -30; "member of another core")]
#[test_case(vec![member(5, -11), member(2, -23)], -23; "own member among others")]
fn a_singleton_culprit_seeds_its_end_member_over_the_hunt_start_offset(
    members: Vec<togi_journal::Member>,
    want: i64,
) {
    let failing = profile(&[(0, -10), (2, -30), (5, -20)]);
    let mut x = Source::new(day(9), &[0, 2, 5], 3, 1).with_context(&context());
    let failure = x.add(super::idle_failure(failing));
    x.add(hunt_start(1, failure, failing, set(&[2])));
    let mut end = culprit(1, set(&[2]));
    end.members = members;
    let end = x.add(end);

    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &[]);
    assert_eq!(
        got.seeds(),
        [seed(2, None, Some(point(want, x.id, end, Signal::Crash)))]
    );
}

#[test]
fn a_hunt_end_seeds_before_any_backoff_decision() {
    let failing = profile(&[(0, -10), (2, -30), (5, -20)]);
    let mut x = Source::new(day(9), &[0, 2, 5], 3, 1).with_context(&context());
    let failure = x.add(super::idle_failure(failing));
    x.add(hunt_start(1, failure, failing, set(&[2])));
    let mut end = culprit(1, set(&[2]));
    end.members = vec![togi_journal::Member {
        core: super::core(2),
        offset: offset(-23),
    }];
    let end = x.add(end);
    let (mut archive, chain) = chain_of(&[&x]);
    let interrupted = pending(&mut archive, &chain, None, &[]);
    let want = [seed(2, None, Some(point(-23, x.id, end, Signal::Crash)))];
    assert_eq!(interrupted.seeds(), want);

    x.add_caused(&[end], backoff_decision(2));
    let (mut archive, chain) = chain_of(&[&x]);
    let decided = pending(&mut archive, &chain, None, &[]);
    assert_eq!(decided.seeds(), want);
}

#[test]
fn resets_drop_seeds() {
    let mut x = Source::new(day(9), &[2], 3, 1).with_context(&context());
    x.pass(2, -30, Condition::Alone);
    x.fail(2, -31, Condition::Alone, Attribution::Attributed);
    x.add(reset(ResetTarget::Core(super::core(2))));
    let failure = x
        .fail(2, -10, Condition::Alone, Attribution::Attributed)
        .failure;

    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &[]);
    assert_eq!(
        got.seeds(),
        [seed(
            2,
            None,
            Some(point(-10, x.id, failure, Signal::UnexpectedExit))
        )]
    );
}

#[test]
fn defect_excluded_failure_not_seeded() {
    let mut x = Source::new(day(9), &[3], 3, 1).with_context(&context());
    let failure = x
        .fail(3, -25, Condition::Alone, Attribution::Attributed)
        .failure;
    x.add_caused(&[failure], backoff_decision(3));

    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &DEFECTS);
    assert_eq!(got.seeds(), []);

    let (mut archive, chain) = chain_of(&[&x]);
    let unregistered = pending(&mut archive, &chain, None, &[]);
    assert_eq!(
        unregistered.seeds(),
        [seed(
            3,
            None,
            Some(point(-25, x.id, failure, Signal::UnexpectedExit))
        )]
    );
}

#[test]
fn excluded_failure_falls_back_to_the_next_shallowest() {
    let mut x = Source::new(day(9), &[3], 3, 1).with_context(&context());
    let deep = x
        .fail(3, -30, Condition::Alone, Attribution::Attributed)
        .failure;
    let shallow = x
        .fail(3, -10, Condition::Alone, Attribution::Attributed)
        .failure;
    x.add_caused(&[shallow], backoff_decision(3));

    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &DEFECTS);
    assert_eq!(
        got.seeds(),
        [seed(
            3,
            None,
            Some(point(-30, x.id, deep, Signal::UnexpectedExit))
        )]
    );
}

#[test_case(-20, Condition::Alone, true; "alone")]
#[test_case(0, Condition::Alone, false; "at CO 0")]
#[test_case(-20, Condition::Together, false; "together")]
fn interrupted_alone_nonzero_seeds_crash_failure(value: i64, condition: Condition, seeded: bool) {
    let mut x = Source::new(day(9), &[4], 3, 1).with_context(&context());
    x.intent(4, value, condition);
    let (mut archive, chain) = chain_of(&[&x]);
    let got = pending(&mut archive, &chain, None, &[]);
    let want: Vec<_> = seeded
        .then(|| seed(4, None, Some(point(-20, x.id, seq(3), Signal::Crash))))
        .into_iter()
        .collect();
    assert_eq!(got.seeds(), want);
}

fn shutdown() -> Shutdown {
    Shutdown {
        reason: ShutdownReason::Signal,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    }
}

#[test]
fn shutdown_stops_in_flight_seed() {
    let mut stopped = Source::new(day(9), &[4], 3, 1).with_context(&context());
    stopped.intent(4, -20, Condition::Alone);
    stopped.add(shutdown());
    let (mut archive, chain) = chain_of(&[&stopped]);
    assert_eq!(pending(&mut archive, &chain, None, &[]).seeds(), []);

    let mut resumed = Source::new(day(9), &[4], 3, 1).with_context(&context());
    resumed.intent(4, -20, Condition::Alone);
    resumed.add(shutdown());
    resumed.intent(4, -22, Condition::Alone);
    let (mut archive, chain) = chain_of(&[&resumed]);
    assert_eq!(
        pending(&mut archive, &chain, None, &[]).seeds(),
        [seed(
            4,
            None,
            Some(point(-22, resumed.id, seq(5), Signal::Crash))
        )]
    );
}

#[test]
fn reset_all_stops_in_flight_seed() {
    let mut cleared = Source::new(day(9), &[4], 3, 1).with_context(&context());
    cleared.intent(4, -20, Condition::Alone);
    cleared.add(reset(ResetTarget::All));
    let (mut archive, chain) = chain_of(&[&cleared]);
    assert_eq!(pending(&mut archive, &chain, None, &[]).seeds(), []);

    let mut resumed = Source::new(day(9), &[4], 3, 1).with_context(&context());
    resumed.intent(4, -20, Condition::Alone);
    resumed.add(reset(ResetTarget::All));
    resumed.intent(4, -22, Condition::Alone);
    let (mut archive, chain) = chain_of(&[&resumed]);
    assert_eq!(
        pending(&mut archive, &chain, None, &[]).seeds(),
        [seed(
            4,
            None,
            Some(point(-22, resumed.id, seq(5), Signal::Crash))
        )]
    );
}

#[test_case(false; "a commitment alone")]
#[test_case(true; "a commitment then a reset of its core")]
fn commitment_stops_seed_walk(reset_after: bool) {
    let mut a = Source::new(day(1), &[5, 7], 2, 1).with_context(&context());
    let failure = a
        .fail(7, -30, Condition::Alone, Attribution::Attributed)
        .failure;
    let mut x = Source::new(day(9), &[5, 7], 3, 1).with_context(&context());
    x.add(SessionCarried {
        sources: vec![carried_source(a.id, 2)],
        failure_points: true,
        detail: None,
        carried: vec![seed(
            5,
            None,
            Some(point(-12, a.id, failure, Signal::Crash)),
        )],
    });
    if reset_after {
        x.add(reset(ResetTarget::Core(super::core(5))));
    }
    let (mut archive, chain) = chain_of(&[&x, &a]);
    let got = pending(&mut archive, &chain, None, &[]);

    assert_eq!(got.sources(), [carried_source(x.id, 3)]);
    let want: Vec<_> = (!reset_after)
        .then(|| seed(5, None, Some(point(-12, a.id, failure, Signal::Crash))))
        .into_iter()
        .collect();
    assert_eq!(got.seeds(), want, "reset {reset_after}");
}

fn unseeded_chain(
    a_context: &togi_machine::BiosContext,
    b_ruleset: u32,
) -> (
    Archive,
    Vec<togi_journal::SessionId>,
    Source,
    Source,
    Source,
) {
    let mut a = Source::new(day(1), &[0, 1, 2], 1, 1).with_context(a_context);
    a.fail(0, -28, Condition::Alone, Attribution::Attributed);
    let mut b = Source::new(day(2), &[0, 1, 2], b_ruleset, 1).with_context(&context());
    b.fail(1, -33, Condition::Alone, Attribution::Attributed);
    let mut x = Source::new(day(9), &[0, 1, 2], 3, 1).with_context(&context());
    x.pass(2, -40, Condition::Alone);
    let (archive, chain) = chain_of(&[&x, &b, &a]);
    (archive, chain, x, b, a)
}

#[test_case(&context(), 2, 3, &[0, 1, 2]; "every transition")]
#[test_case(&other_context(), 2, 2, &[1, 2]; "BIOS changed before B")]
#[test_case(&context(), 3, 1, &[2]; "B ran the same ruleset")]
fn walk_back_through_unseeded_transitions(
    a_context: &togi_machine::BiosContext,
    b_ruleset: u32,
    sources: usize,
    cores: &[u8],
) {
    let (mut archive, chain, ..) = unseeded_chain(a_context, b_ruleset);
    let got = pending(&mut archive, &chain, None, &[]);
    assert_eq!(got.sources().len(), sources);
    let seeded: Vec<_> = got.seeds().iter().map(|seed| seed.core.get()).collect();
    assert_eq!(seeded, cores);
}

#[test]
fn reset_all_in_source_stops_walk() {
    let mut a = Source::new(day(1), &[0, 1, 2], 1, 1).with_context(&context());
    a.fail(0, -28, Condition::Alone, Attribution::Attributed);
    let mut b = Source::new(day(2), &[0, 1, 2], 2, 1).with_context(&context());
    b.fail(1, -33, Condition::Alone, Attribution::Attributed);
    b.pass(0, -20, Condition::Alone);
    b.add(reset(ResetTarget::All));
    let after = b
        .fail(1, -10, Condition::Alone, Attribution::Attributed)
        .failure;
    let mut x = Source::new(day(9), &[0, 1, 2], 3, 1).with_context(&context());
    let solo = x.pass(2, -40, Condition::Alone);
    let (mut archive, chain) = chain_of(&[&x, &b]);
    archive.poison(a.id);
    let chain = [chain, vec![a.id]].concat();

    let got = pending(&mut archive, &chain, None, &[]);

    assert_eq!(got.sources().len(), 2);
    assert_eq!(
        got.seeds(),
        [
            seed(
                1,
                None,
                Some(point(-10, b.id, after, Signal::UnexpectedExit))
            ),
            seed(2, Some(limit(-40, x.id, solo)), None),
        ]
    );
    assert!(
        !archive.opened.contains(&a.id),
        "the walk read behind an all-reset"
    );
}

#[test]
fn reset_all_in_the_marked_source_drops_its_earlier_seeds_and_stops() {
    let mut older = Source::new(day(1), &[0], 1, 1).with_context(&context());
    older.fail(0, -28, Condition::Alone, Attribution::Attributed);
    let mut x = Source::new(day(9), &[0], 3, 1).with_context(&context());
    x.pass(0, -40, Condition::Alone);
    x.add(reset(ResetTarget::All));
    let (mut archive, chain) = chain_of(&[&x]);
    archive.poison(older.id);
    let chain = [chain, vec![older.id]].concat();
    let got = pending(&mut archive, &chain, None, &[]);
    assert_eq!(got.seeds(), []);
    assert_eq!(got.sources().len(), 1);
}

#[test]
fn settled_source_not_seeded_after_core_phase() {
    let id = day(1);
    let live = |extra: Option<Payload>| {
        let mut b = Source::new(day(2), &[0], 4, 1).with_context(&context());
        if let Some(extra) = extra {
            b.add(extra);
        }
        b.events
    };
    let commitment = |named: togi_journal::SessionId| {
        Payload::SessionCarried(SessionCarried {
            sources: vec![carried_source(named, 3)],
            failure_points: true,
            detail: None,
            carried: Vec::new(),
        })
    };
    let phase = Payload::CorePhase(CoreTransition {
        core: super::core(0),
        from: None,
        to: CorePhase::Search,
        offset: offset(-5),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: vec![workload(togi_machine::Regime::R1, 0)],
        cleared_combination: Vec::new(),
        reason: "start".into(),
    });
    assert!(!settled(id, &live(None)));
    assert!(settled(id, &live(Some(commitment(id)))));
    assert!(!settled(id, &live(Some(commitment(day(5))))));
    assert!(settled(id, &live(Some(phase))));
}

#[test]
fn walk_stops_at_older_commitment() {
    let mut a = Source::new(day(1), &[0, 7], 1, 1).with_context(&context());
    let solo = a.pass(0, -30, Condition::Alone);
    a.fail(7, -20, Condition::Alone, Attribution::Attributed);
    let mut b = Source::new(day(2), &[0, 7], 2, 1).with_context(&context());
    b.add(SessionCarried {
        sources: vec![carried_source(a.id, 1)],
        failure_points: true,
        detail: None,
        carried: vec![seed(0, Some(limit(-30, a.id, solo)), None)],
    });
    let c = Source::new(day(3), &[0, 7], 3, 1).with_context(&context());
    let (mut archive, chain) = chain_of(&[&c, &b, &a]);
    let got = pending(&mut archive, &chain, None, &[]);

    assert_eq!(
        got.sources(),
        [carried_source(c.id, 3), carried_source(b.id, 2)]
    );
    assert_eq!(got.seeds(), [seed(0, Some(limit(-30, a.id, solo)), None)]);
}

#[test]
fn seed_extrema_tie_keeps_first() {
    let mut older = Source::new(day(1), &[0], 1, 1).with_context(&context());
    older.pass(0, -30, Condition::Alone);
    older.fail(0, -10, Condition::Alone, Attribution::Attributed);
    let mut newer = Source::new(day(9), &[0], 2, 1).with_context(&context());
    let solo = newer.pass(0, -30, Condition::Alone);
    newer.pass(0, -30, Condition::Alone);
    let failure = newer
        .fail(0, -10, Condition::Alone, Attribution::Attributed)
        .failure;
    newer.fail(0, -10, Condition::Alone, Attribution::Attributed);

    let (mut archive, chain) = chain_of(&[&newer, &older]);
    let got = pending(&mut archive, &chain, None, &[]);
    assert_eq!(
        got.seeds(),
        [seed(
            0,
            Some(limit(-30, newer.id, solo)),
            Some(point(-10, newer.id, failure, Signal::UnexpectedExit)),
        )]
    );

    let mut deeper = Source::new(day(1), &[0], 1, 1).with_context(&context());
    let deepest = deeper.pass(0, -31, Condition::Alone);
    let shallowest = deeper
        .fail(0, -9, Condition::Alone, Attribution::Attributed)
        .failure;
    let (mut archive, chain) = chain_of(&[&newer, &deeper]);
    let got = pending(&mut archive, &chain, None, &[]);
    assert_eq!(
        got.seeds(),
        [seed(
            0,
            Some(limit(-31, deeper.id, deepest)),
            Some(point(-9, deeper.id, shallowest, Signal::UnexpectedExit)),
        )]
    );
}

#[test_case(togi_journal::HuntOutcome::Direct, set(&[2]), true; "direct")]
#[test_case(togi_journal::HuntOutcome::Culprit, set(&[2, 5]), true; "two culprits")]
#[test_case(togi_journal::HuntOutcome::Culprit, set(&[2]), false; "no hunt start")]
fn only_a_singleton_culprit_of_a_known_hunt_seeds(
    outcome: togi_journal::HuntOutcome,
    culprits: togi_machine::CoreSet,
    start_hunt: bool,
) {
    let failing = profile(&[(0, -10), (2, -35), (5, -20)]);
    let mut x = Source::new(day(9), &[0, 2, 5], 3, 1).with_context(&context());
    let failure = x.add(super::idle_failure(failing));
    if start_hunt {
        x.add(hunt_start(1, failure, failing, set(&[2])));
    }
    let mut end = culprit(1, culprits);
    end.result = outcome;
    x.add(end);

    let (mut archive, chain) = chain_of(&[&x]);
    assert_eq!(pending(&mut archive, &chain, None, &[]).seeds(), []);
}
