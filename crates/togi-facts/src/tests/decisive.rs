use std::collections::BTreeMap;
use std::num::NonZeroU32;

use togi_journal::{
    Activity, ConfigLoaded, Outcome, Payload, SessionContext, Shutdown, ShutdownReason, TrialEnd,
};
use togi_machine::{BootId, CcdId, Condition, Regime, Signal};

use super::{
    Script, alone, build, core, end, fail, first, intent, pass, profile, set, trial, workload,
};
use crate::{Decisive, Observation, TrialClass};

fn config_loaded(evidence: u32) -> ConfigLoaded {
    let mut stamp = build(evidence);
    stamp.ruleset = 6;
    ConfigLoaded {
        build: stamp,
        config: togi_config::Config::default(),
        path: "config.json".into(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    }
}

#[test]
fn decisive_facts() {
    let mut s = Script::begin(&[4, 2]);
    s.add(SessionContext {
        bios_version: "1.2".into(),
        board: "board".into(),
        cpu_model: "cpu".into(),
        microcode: "0".into(),
        boost_limit_mhz: 5000,
    });
    s.add(Payload::from(togi_journal::TrialIntent {
        activity: Some(Activity::Search),
        ..alone("alone", 4, -10, &[2, 4])
    }));
    let alone_end = s.add(pass("alone", 89));
    s.add(togi_journal::TrialIntent {
        cores: set(&[4, 2]),
        profile: profile(&[(2, -20), (4, -30)]),
        duration_s: 900,
        activity: Some(Activity::Checking),
        ..intent("together", Condition::Together, Regime::R7)
    });
    s.boot("b");
    s.add(config_loaded(2));
    let together_end = s.add(fail("together", Signal::Crash, 12));
    s.add(togi_journal::TrialIntent {
        cores: set(&[2, 4]),
        profile: profile(&[(2, -9), (4, -30)]),
        duration_s: 120,
        rerun: true,
        ..intent("parked", Condition::Parked, Regime::R7)
    });
    let parked_end = s.add(pass("parked", 120));
    s.add(intent("inconclusive", Condition::Alone, Regime::R2));
    s.add(end("inconclusive", Outcome::Inconclusive, 1));
    s.add(pass("unknown", 5));
    s.add(intent("open", Condition::Alone, Regime::R2));

    let facts = s.extract();
    assert_eq!(facts.facts.len(), 3);
    assert_eq!(facts.epoch(), 1);
    assert_eq!(facts.context.as_ref().unwrap().bios_version, "1.2");
    assert_eq!(facts.open_trials.len(), 1);

    let [alone_fact, together_fact, parked_fact] = &facts.facts[..] else {
        unreachable!()
    };
    assert_eq!(alone_fact.source.seq, alone_end);
    assert_eq!(alone_fact.local, alone_end);
    assert_eq!(alone_fact.source.time, Script::time(alone_end));
    assert_eq!(alone_fact.source.session, first());
    assert_eq!(alone_fact.source.build, build(1));
    assert_eq!(alone_fact.source.epoch, 1);
    assert_eq!(alone_fact.source.boot, BootId::from("a".to_owned()));
    assert_eq!(alone_fact.source.trial, Some(trial("alone")));
    assert_eq!(alone_fact.source.backend, None);
    assert_eq!(
        alone_fact.observation,
        Observation::Trial {
            class: TrialClass {
                workload: workload(Regime::R1),
                loaded: set(&[4]),
                duration_s: 90,
            },
            condition: Condition::Alone,
            activity: Some(Activity::Search),
            rerun: false,
            profile: profile(&[(2, 0), (4, -10)]),
            outcome: Decisive::Pass,
            duration_s: 89,
            telemetry: None,
        }
    );

    assert_eq!(together_fact.source.seq, together_end);
    assert_eq!(
        together_fact.source.build,
        build(1),
        "build is the intent's"
    );
    assert_eq!(together_fact.source.epoch, 1);
    assert_eq!(together_fact.source.boot, BootId::from("a".to_owned()));
    let Observation::Trial {
        class,
        outcome,
        duration_s,
        profile: applied,
        ..
    } = &together_fact.observation
    else {
        unreachable!()
    };
    assert_eq!(class.loaded, set(&[2, 4]));
    assert_eq!(class.duration_s, 900, "class carries the intended duration");
    assert_eq!(*duration_s, 12, "the fact carries the measured duration");
    assert_eq!(*applied, profile(&[(2, -20), (4, -30)]));
    assert_eq!(
        *outcome,
        Decisive::Failure {
            signal: Signal::Crash,
            core: None,
            stalled: None,
        }
    );

    assert_eq!(parked_fact.source.seq, parked_end);
    assert_eq!(
        parked_fact.source.build.evidence, 2,
        "intent after config.loaded"
    );
    assert_eq!(
        parked_fact.source.epoch, 1,
        "epoch comes from session.start only"
    );
    assert_eq!(parked_fact.source.boot, BootId::from("b".to_owned()));
    let Observation::Trial { rerun, .. } = &parked_fact.observation else {
        unreachable!()
    };
    assert!(rerun);
}

#[test]
fn failure_keeps_attribution_and_telemetry() {
    let mut s = Script::begin(&[1, 3, 7]);
    s.add(togi_journal::TrialIntent {
        cores: set(&[7, 3]),
        profile: profile(&[(1, -20), (3, -30), (7, -40)]),
        duration_s: 120,
        ..intent("0304", Condition::Together, Regime::R7)
    });
    s.add(TrialEnd {
        core: Some(core(7)),
        stalled_core: Some(core(3)),
        voltage_requests_v: [(core(3), 1.125), (core(7), 1.25)].into_iter().collect(),
        top_requesters: set(&[7]),
        ccd_mhz: BTreeMap::from([(CcdId(0), 4800)]),
        ..fail("0304", Signal::ComputationError, 11)
    });
    let facts = s.extract();
    let Observation::Trial {
        outcome, telemetry, ..
    } = &facts.facts[0].observation
    else {
        unreachable!()
    };
    assert_eq!(
        *outcome,
        Decisive::Failure {
            signal: Signal::ComputationError,
            core: Some(core(7)),
            stalled: Some(core(3)),
        }
    );
    let telemetry = telemetry.as_ref().unwrap();
    assert_eq!(telemetry.top_requesters, set(&[7]));
    assert_eq!(telemetry.ccd_mhz, BTreeMap::from([(CcdId(0), 4800)]));
    assert_eq!(telemetry.voltage_requests_v.get(core(3)), Some(1.125));
}

#[test]
fn failure_without_signal_is_incomplete() {
    let mut s = Script::begin(&[0]);
    s.add(alone("one", 0, -10, &[0]));
    s.add(end("one", Outcome::Failure, 5));
    assert!(matches!(
        s.try_extract(first()),
        Err(crate::FactsError::Incomplete {
            field: "signal",
            ..
        })
    ));
}

#[test]
fn duplicate_end_is_error() {
    let mut s = Script::begin(&[0]);
    s.add(alone("one", 0, -10, &[0]));
    s.add(pass("one", 5));
    s.add(fail("one", Signal::Crash, 6));
    assert!(matches!(
        s.try_extract(first()),
        Err(crate::FactsError::DuplicateEnd { trial: t, .. }) if t == trial("one")
    ));
}

#[test]
fn late_inconclusive_end_after_decisive_is_ignored() {
    let mut s = Script::begin(&[0]);
    s.add(alone("one", 0, -10, &[0]));
    s.add(pass("one", 5));
    s.add(end("one", Outcome::Inconclusive, 6));
    assert_eq!(s.extract().facts.len(), 1);
}

#[test]
fn duplicate_intent_is_error() {
    let mut s = Script::begin(&[0]);
    s.add(alone("one", 0, -10, &[0]));
    s.add(alone("one", 0, -10, &[0]));
    assert!(matches!(
        s.try_extract(first()),
        Err(crate::FactsError::DuplicateIntent { .. })
    ));
}

#[test]
fn stream_must_begin_with_one_matching_session_start() {
    let mut late = Script::new("a");
    late.add(alone("one", 0, -10, &[0]));
    assert!(matches!(
        late.try_extract(first()),
        Err(crate::FactsError::NotStarted)
    ));

    let empty = Script::new("a");
    assert!(matches!(
        empty.try_extract(first()),
        Err(crate::FactsError::NotStarted)
    ));

    let mut twice = Script::begin(&[0]);
    twice.add(super::start(first(), &[0], 1));
    assert!(matches!(
        twice.try_extract(first()),
        Err(crate::FactsError::DuplicateStart { .. })
    ));

    let other = Script::begin(&[0]);
    assert!(matches!(
        other.try_extract(super::session("20261002T000000Z")),
        Err(crate::FactsError::SessionMismatch { .. })
    ));
}

#[test]
fn empty_topology_rejected() {
    let mut s = Script::new("a");
    s.add(super::start(first(), &[], 1));
    assert!(matches!(
        s.try_extract(first()),
        Err(crate::FactsError::EmptyTopology)
    ));
}

#[test]
fn stamps_and_markers_come_from_their_own_events() {
    let mut s = Script::begin(&[0]);
    s.add(config_loaded(9));
    let carried = s.add(togi_journal::SessionCarried {
        sources: Vec::new(),
        failure_points: true,
        detail: None,
        carried: Vec::new(),
    });
    s.add(togi_journal::SessionCarried {
        sources: Vec::new(),
        failure_points: true,
        detail: None,
        carried: Vec::new(),
    });
    s.add(Shutdown {
        reason: ShutdownReason::Signal,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    });
    let latest = s.add(Shutdown {
        reason: ShutdownReason::Command,
        kernel_cursor: None,
        kernel_error: None,
        cycles: NonZeroU32::new(1),
    });
    let facts = s.extract();
    assert_eq!(facts.build, build(1));
    assert_eq!(facts.epoch(), 1);
    assert_eq!(facts.carry_commit, Some(carried));
    assert_eq!(facts.shutdown, Some(latest));
    assert_eq!(facts.topology.len(), 1);
    assert_eq!(facts.cores, set(&[0]));
}

#[test]
fn own_pass_takes_the_backend_recorded_before_its_intent() {
    let loaded = |mprime: &str, ycruncher: &str| ConfigLoaded {
        config: togi_config::Config::parse(&format!(
            r#"{{"backends":{{"mprime":"{mprime}","ycruncher":"{ycruncher}"}}}}"#
        ))
        .unwrap(),
        ..config_loaded(1)
    };
    let mut s = Script::begin(&[0]);
    s.add(loaded("/store/m1", "/store/y1"));
    s.add(alone("pass", 0, -10, &[0]));
    s.add(loaded("/store/m2", "/store/y2"));
    s.add(pass("pass", 5));
    s.add(alone("fail", 0, -10, &[0]));
    s.add(fail("fail", Signal::Crash, 5));
    s.add(alone("later", 0, -10, &[0]));
    s.add(pass("later", 5));
    let facts = s.extract();
    let backend = workload(Regime::R1).backend();
    let path = |n: &str| match backend {
        togi_machine::Backend::Mprime => format!("/store/m{n}"),
        togi_machine::Backend::Ycruncher => format!("/store/y{n}"),
    };
    let identity = |n: &str| {
        Some(crate::BackendIdentity {
            backend,
            path: path(n),
        })
    };
    assert_eq!(facts.facts[0].source.backend, identity("1"));
    assert_eq!(
        facts.facts[1].source.backend, None,
        "failures are not keyed by backend"
    );
    assert_eq!(facts.facts[2].source.backend, identity("2"));
}
