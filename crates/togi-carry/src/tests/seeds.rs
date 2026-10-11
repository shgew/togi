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
fn bios_change_archives_metadata_carry_available() {
    for (recorded, current, kept, detail) in [
        (Some(context()), context(), true, None),
        (
            Some(context()),
            other_context(),
            false,
            Some("bios_version is 3.20; the session recorded 3.10"),
        ),
        (
            None,
            other_context(),
            false,
            Some("the archived session recorded no BIOS context"),
        ),
    ] {
        let mut x = Source::new(day(9), &[0, 1], 4, 1);
        if let Some(recorded) = &recorded {
            x = x.with_context(recorded);
        }
        let failure = x
            .fail(0, -30, Condition::Alone, Attribution::Attributed)
            .failure;
        let solo = x.pass(1, -20, Condition::Alone);
        let (mut archive, chain) = chain_of(&[&x]);
        let got = pending(&mut archive, &chain, None, &[]);
        let resolved = got.resolve(&current, &togi_config::Backends::default(), 1);

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

#[test]
fn hunt_culprit_seed_sparse_topology() {
    for (name, reset_after, attributed, want) in [
        ("culprit", false, 0, Some((-35, true))),
        ("reset culprit", true, 0, None),
        (
            "attributed failure is shallower",
            false,
            -28,
            Some((-28, false)),
        ),
        ("culprit is shallower", false, -40, Some((-35, true))),
    ] {
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
        assert_eq!(got.seeds(), want, "{name}");
    }
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

#[test]
fn interrupted_alone_nonzero_seeds_crash_failure() {
    for (name, value, condition, seeded) in [
        ("alone", -20, Condition::Alone, true),
        ("at CO 0", 0, Condition::Alone, false),
        ("together", -20, Condition::Together, false),
    ] {
        let mut x = Source::new(day(9), &[4], 3, 1).with_context(&context());
        x.intent(4, value, condition);
        let (mut archive, chain) = chain_of(&[&x]);
        let got = pending(&mut archive, &chain, None, &[]);
        let want: Vec<_> = seeded
            .then(|| seed(4, None, Some(point(-20, x.id, seq(3), Signal::Crash))))
            .into_iter()
            .collect();
        assert_eq!(got.seeds(), want, "{name}");
    }
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
fn commitment_stops_seed_walk() {
    for reset_after in [false, true] {
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

#[test]
fn walk_back_through_unseeded_transitions() {
    for (name, a_context, b_ruleset, sources, cores) in [
        ("every transition", context(), 2, 3, vec![0, 1, 2]),
        ("BIOS changed before B", other_context(), 2, 2, vec![1, 2]),
        ("B ran the same ruleset", context(), 3, 1, vec![2]),
    ] {
        let (mut archive, chain, ..) = unseeded_chain(&a_context, b_ruleset);
        let got = pending(&mut archive, &chain, None, &[]);
        assert_eq!(got.sources().len(), sources, "{name}");
        let seeded: Vec<_> = got.seeds().iter().map(|seed| seed.core.get()).collect();
        let want: Vec<_> = cores.iter().map(|&n| u8::try_from(n).unwrap()).collect();
        assert_eq!(seeded, want, "{name}");
    }
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
