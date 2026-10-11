use std::collections::BTreeMap;
use std::num::NonZeroU32;

use togi_journal::{
    BackendIdentity, ConfigLoaded, FailureCarried, Outcome, Payload, TrialCarried, TrialEnd,
};
use togi_machine::{Backend, CcdId, Condition, Regime, Signal};

use super::{
    Script, alone, build, core, fail, idle, intent, pass, profile, session, set, start, trial,
};
use crate::{Fact, FactsError, Observation, SessionFacts};

fn original() -> SessionFacts {
    let mut stamp = build(4);
    stamp.ruleset = 6;
    let mut s = Script::new("intent-boot");
    s.add(togi_journal::SessionStart {
        build: stamp,
        ..start(super::first(), &[7, 1, 3], 4)
    });
    s.add(togi_journal::TrialIntent {
        cores: set(&[7, 3]),
        profile: profile(&[(1, -20), (3, -30), (7, -40)]),
        duration_s: 120,
        rerun: true,
        ..intent("0304", Condition::Together, Regime::R7)
    });
    s.add(ConfigLoaded {
        build: build(4),
        config: togi_config::Config::default(),
        path: "later".into(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    });
    s.boot("end-boot");
    s.add(TrialEnd {
        core: Some(core(7)),
        stalled_core: Some(core(3)),
        voltage_requests_v: [(core(3), 1.125), (core(7), 1.25)].into_iter().collect(),
        top_requesters: set(&[7]),
        ccd_mhz: BTreeMap::from([(CcdId(0), 4800)]),
        ..fail("0304", Signal::ComputationError, 11)
    });
    s.boot("idle-boot");
    let mut failure = idle(
        Some(Condition::Parked),
        Some(profile(&[(1, -20), (3, -30), (7, -40)])),
    );
    failure.core = Some(core(3));
    failure.offset = Some(super::offset(-30));
    failure.regime = Some(Regime::R7);
    failure.reason = Some("idle".into());
    failure.round = NonZeroU32::new(2);
    s.add(failure);
    s.extract()
}

fn copy_into(
    id: &str,
    evidence: u32,
    copies: &[Fact],
    tweak: impl Fn(&mut Payload),
) -> SessionFacts {
    let id = session(id);
    let mut s = Script::new("copy-boot");
    s.add(start(id, &[7, 1, 3], evidence));
    for fact in copies {
        let mut payload = fact.to_payload();
        tweak(&mut payload);
        s.add(payload);
    }
    s.boot("own-boot");
    s.add(togi_journal::TrialIntent {
        profile: profile(&[(1, -10), (3, 0), (7, 0)]),
        core: Some(core(1)),
        offset: Some(super::offset(-10)),
        ..intent("own", Condition::Alone, Regime::R1)
    });
    s.add(pass("own", 5));
    s.try_extract(id).unwrap()
}

#[test]
fn carried_facts_keep_original_provenance() {
    let first = original();
    assert_eq!(first.facts.len(), 2);
    let Observation::Trial {
        class, duration_s, ..
    } = &first.facts[0].observation
    else {
        unreachable!()
    };
    assert_eq!(class.loaded, set(&[3, 7]));
    assert_eq!(class.duration_s, 120);
    assert_eq!(*duration_s, 11);
    assert_eq!(first.facts[0].source.build.ruleset, 6);
    assert_eq!(first.facts[0].source.boot.as_str(), "intent-boot");
    assert_eq!(first.facts[0].source.epoch, 4);

    let backend = BackendIdentity {
        backend: Backend::Mprime,
        path: "/store/mprime".into(),
    };
    let stamp = backend.clone();
    let mut expected = first.facts.clone();
    expected[0].source.backend = Some(backend);

    let second = copy_into("20261002T000000Z", 9, &first.facts, |payload| {
        if let Payload::TrialCarried(carried) = payload {
            carried.source.backend = Some(stamp.clone());
        }
    });
    assert_eq!(second.epoch(), 9);
    assert_eq!(second.facts.len(), 1);
    assert_eq!(second.facts[0].source.session, session("20261002T000000Z"));
    assert_eq!(second.facts[0].source.trial, Some(trial("own")));
    assert_eq!(second.facts[0].source.epoch, 9);
    assert_eq!(second.carried.len(), 2);
    for (copy, original) in second.carried.iter().zip(&expected) {
        assert_eq!(copy.source, original.source);
        assert_eq!(copy.observation, original.observation);
        assert_eq!(copy.id(), original.id());
    }
    assert_eq!(second.carried[0].local.get(), 2);
    assert_eq!(second.carried[1].local.get(), 3);

    let third = copy_into("20261003T000000Z", 11, &second.carried, |_| {});
    assert_eq!(third.carried.len(), 2);
    for (copy, original) in third.carried.iter().zip(&expected) {
        assert_eq!(copy.source, original.source);
        assert_eq!(copy.observation, original.observation);
    }
    assert_eq!(third.facts[0].source.epoch, 11);
    assert!(third.carried.iter().all(|fact| fact.source.epoch != 11));
}

#[test]
fn carried_payload_round_trip_matches_the_recorded_idle_failure() {
    let first = original();
    let Payload::FailureCarried(carried) = first.facts[1].to_payload() else {
        unreachable!()
    };
    assert_eq!(carried.failure.core, Some(core(3)));
    assert_eq!(carried.failure.offset, Some(super::offset(-30)));
    assert_eq!(carried.failure.regime, Some(Regime::R7));
    assert_eq!(carried.failure.reason.as_deref(), Some("idle"));
    assert_eq!(carried.failure.round, NonZeroU32::new(2));
}

#[test]
fn carried_facts_must_be_decisive_well_formed() {
    let first = original();
    let Payload::TrialCarried(trial_copy) = first.facts[0].to_payload() else {
        unreachable!()
    };
    let Payload::FailureCarried(idle_copy) = first.facts[1].to_payload() else {
        unreachable!()
    };
    let cases: Vec<(Payload, &str)> = vec![
        (
            Payload::TrialCarried(TrialCarried {
                outcome: Outcome::Inconclusive,
                ..trial_copy.clone()
            }),
            "inconclusive",
        ),
        (
            Payload::TrialCarried(TrialCarried {
                signal: None,
                ..trial_copy.clone()
            }),
            "signal",
        ),
        (
            Payload::TrialCarried(TrialCarried {
                class: togi_journal::TrialClass {
                    workload: None,
                    ..trial_copy.class.clone()
                },
                ..trial_copy
            }),
            "workload",
        ),
        (
            Payload::FailureCarried(FailureCarried {
                failure: togi_journal::Failure {
                    trial: Some(trial("x")),
                    ..idle_copy.failure.clone()
                },
                ..idle_copy.clone()
            }),
            "not idle",
        ),
        (
            Payload::FailureCarried(FailureCarried {
                failure: togi_journal::Failure {
                    profile: None,
                    ..idle_copy.failure.clone()
                },
                ..idle_copy
            }),
            "profile",
        ),
    ];
    for (payload, name) in cases {
        let mut s = Script::begin(&[7, 1, 3]);
        s.add(payload);
        let result = s.try_extract(super::first());
        assert!(
            matches!(
                (name, &result),
                ("inconclusive", Err(FactsError::NotDecisive { .. }))
                    | (
                        "signal" | "workload" | "profile",
                        Err(FactsError::Incomplete { .. })
                    )
                    | ("not idle", Err(FactsError::NotIdle { .. }))
            ),
            "{name}: {result:?}"
        );
    }
}

#[test]
fn a_passing_copy_round_trips_and_accessors_agree() {
    let mut s = Script::begin(&[0, 1]);
    s.add(alone("p", 1, -7, &[0, 1]));
    s.add(pass("p", 9));
    s.add(idle(
        Some(Condition::Together),
        Some(profile(&[(0, -1), (1, -2)])),
    ));
    let facts = s.extract();
    let [pass_fact, idle_fact] = &facts.facts[..] else {
        unreachable!()
    };
    assert!(!pass_fact.is_failure());
    assert!(idle_fact.is_failure());
    assert_eq!(pass_fact.regime(), Regime::R1);
    assert_eq!(pass_fact.loaded(), set(&[1]));
    assert_eq!(idle_fact.profile(), &profile(&[(0, -1), (1, -2)]));
    assert_eq!(pass_fact.id().session, super::first());
    assert_eq!(pass_fact.id().seq, pass_fact.source.seq);

    let mut copy = Script::begin(&[0, 1]);
    copy.add(pass_fact.to_payload());
    copy.add(idle_fact.to_payload());
    let copied = copy.extract();
    assert_eq!(copied.carried.len(), 2);
    for (copy, original) in copied.carried.iter().zip(&facts.facts) {
        assert_eq!(copy.source, original.source);
        assert_eq!(copy.observation, original.observation);
    }
}
