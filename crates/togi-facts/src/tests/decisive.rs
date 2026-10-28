use test_case::test_case;

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use togi_journal::{
    Activity, ConfigLoaded, Outcome, Payload, SessionContext, Shutdown, ShutdownReason, TrialEnd,
};
use togi_machine::{BootId, CcdId, Condition, Regime, Signal};

use super::{
    Script, alone, build, config_loaded, config_with_backends, core, end, fail, first, idle,
    intent, pass, profile, refreshed, set, trial, workload,
};
use crate::{Decisive, FactsError, Observation, Telemetry, TrialClass};

#[test]
fn an_excluded_pass_cannot_be_carried_as_a_full_shape_fact() {
    let mut s = Script::begin(&[0, 1]);
    s.add(togi_journal::TrialIntent {
        task_start: None,
        cores: set(&[0, 1]),
        profile: profile(&[(0, -10), (1, -10)]),
        ..intent("excluded", Condition::Together, Regime::R7)
    });
    s.add(TrialEnd {
        excluded: vec![core(0)],
        ..pass("excluded", 90)
    });
    let facts = s.extract();
    assert_eq!(facts.facts.len(), 0);
    assert_eq!(facts.open_trials.len(), 0);
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
        task_start: None,
        activity: Some(Activity::Search),
        ..alone("alone", 4, -10, &[2, 4])
    }));
    let alone_end = s.add(pass("alone", 89));
    s.add(togi_journal::TrialIntent {
        task_start: None,
        cores: set(&[4, 2]),
        profile: profile(&[(2, -20), (4, -30)]),
        duration_s: 900,
        activity: Some(Activity::Checking),
        ..intent("together", Condition::Together, Regime::R7)
    });
    s.boot("b");
    let refreshed_build = refreshed(&build(1), "0.2.0");
    s.add(config_loaded(refreshed_build.clone()));
    let together_end = s.add(fail("together", Signal::Crash, 12));
    s.add(togi_journal::TrialIntent {
        task_start: None,
        cores: set(&[2, 4]),
        profile: profile(&[(2, -9), (4, -30)]),
        duration_s: 120,
        rerun: true,
        ..intent("parked", Condition::Parked, Regime::R7)
    });
    let parked_end = s.add(pass("parked", 120));
    s.add(intent("inconclusive", Condition::Alone, Regime::R2));
    s.add(end("inconclusive", Outcome::Inconclusive, 1));
    s.add(togi_journal::TrialIntent {
        task_start: None,
        cores: set(&[2, 4]),
        core: Some(core(4)),
        profile: profile(&[(2, 0), (4, -30)]),
        duration_s: 60,
        rerun: true,
        ..intent("single", Condition::Together, Regime::R2)
    });
    let single_end = s.add(pass("single", 60));
    s.add(pass("unknown", 5));
    s.add(intent("open", Condition::Alone, Regime::R2));

    let facts = s.extract();
    assert_eq!(facts.facts.len(), 4);
    assert_eq!(facts.epoch(), 1);
    assert!(
        facts
            .facts
            .iter()
            .all(|fact| fact.source.context.as_ref().unwrap().bios_version == "1.2")
    );
    assert_eq!(facts.open_trials.len(), 1);

    let [alone_fact, together_fact, parked_fact, single_fact] = &facts.facts[..] else {
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
    assert_eq!(
        together_fact.observation,
        Observation::Trial {
            class: TrialClass {
                workload: workload(Regime::R7),
                loaded: set(&[2, 4]),
                duration_s: 900,
            },
            condition: Condition::Together,
            activity: Some(Activity::Checking),
            rerun: false,
            profile: profile(&[(2, -20), (4, -30)]),
            outcome: Decisive::Failure {
                signal: Signal::Crash,
                core: None,
                stalled: None,
            },
            duration_s: 12,
            telemetry: None,
        },
        "class carries the intended duration, the fact the measured one"
    );

    assert_eq!(parked_fact.source.seq, parked_end);
    assert_eq!(
        parked_fact.source.build, refreshed_build,
        "intent after config.loaded takes the refreshed build"
    );
    assert_eq!(
        parked_fact.source.epoch, 1,
        "epoch comes from session.start only"
    );
    assert_eq!(parked_fact.source.boot, BootId::from("b".to_owned()));
    assert_eq!(
        parked_fact.observation,
        Observation::Trial {
            class: TrialClass {
                workload: workload(Regime::R7),
                loaded: set(&[2, 4]),
                duration_s: 120,
            },
            condition: Condition::Parked,
            activity: None,
            rerun: true,
            profile: profile(&[(2, -9), (4, -30)]),
            outcome: Decisive::Pass,
            duration_s: 120,
            telemetry: None,
        }
    );

    assert_eq!(single_fact.source.seq, single_end);
    assert_eq!(
        single_fact.observation,
        Observation::Trial {
            class: TrialClass {
                workload: workload(Regime::R2),
                loaded: set(&[4]),
                duration_s: 60,
            },
            condition: Condition::Together,
            activity: None,
            rerun: true,
            profile: profile(&[(2, 0), (4, -30)]),
            outcome: Decisive::Pass,
            duration_s: 60,
            telemetry: None,
        },
        "a single-core rerun is classed by that one core"
    );
}

#[test_case("schema", |b| b.schema += 1; "schema")]
#[test_case("ruleset", |b| b.ruleset += 1; "ruleset")]
#[test_case("evidence", |b| b.evidence += 1; "evidence")]
fn config_loaded_changing_a_durable_stamp_is_an_error(
    name: &str,
    change: fn(&mut togi_journal::Build),
) {
    let mut stamp = refreshed(&build(1), "0.2.0");
    change(&mut stamp);
    let mut s = Script::begin(&[0]);
    let seq = s.add(config_loaded(stamp));
    let result = s.try_extract(first());
    assert!(
        matches!(&result, Err(FactsError::StampChanged { seq: at, field }) if *at == seq && *field == name),
        "{name}: {result:?}"
    );
}

#[test]
fn failure_keeps_attribution_and_telemetry() {
    let mut s = Script::begin(&[1, 3, 7]);
    s.add(togi_journal::TrialIntent {
        task_start: None,
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
        ccd_mhz: BTreeMap::from([(CcdId::new(0).unwrap(), 4800)]),
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
    assert_eq!(
        *telemetry,
        Some(Telemetry {
            voltage_requests_v: [(core(3), 1.125), (core(7), 1.25)].into_iter().collect(),
            top_requesters: set(&[7]),
            ccd_mhz: BTreeMap::from([(CcdId::new(0).unwrap(), 4800)]),
        })
    );
    let telemetry = telemetry.as_ref().unwrap();
    assert_eq!(telemetry.voltage_requests_v.get(core(3)), Some(1.125));
    assert_eq!(telemetry.voltage_requests_v.get(core(7)), Some(1.25));
}

#[test_case(TrialEnd { voltage_requests_v: [(core(0), 1.0625)].into_iter().collect(), ..pass("t", 5) },
    &Telemetry { voltage_requests_v: [(core(0), 1.0625)].into_iter().collect(), top_requesters: set(&[]), ccd_mhz: BTreeMap::new() }; "voltage requests")]
#[test_case(TrialEnd { top_requesters: set(&[0]), ..pass("t", 5) },
    &Telemetry { voltage_requests_v: togi_machine::PerCore::default(), top_requesters: set(&[0]), ccd_mhz: BTreeMap::new() }; "top requesters")]
#[test_case(TrialEnd { ccd_mhz: BTreeMap::from([(CcdId::new(0).unwrap(), 4700)]), ..pass("t", 5) },
    &Telemetry { voltage_requests_v: togi_machine::PerCore::default(), top_requesters: set(&[]), ccd_mhz: BTreeMap::from([(CcdId::new(0).unwrap(), 4700)]) }; "ccd mhz")]
fn telemetry_is_present_when_any_one_lane_is_populated(trial_end: TrialEnd, want: &Telemetry) {
    let mut s = Script::begin(&[0]);
    s.add(alone("t", 0, -5, &[0]));
    s.add(trial_end);
    let facts = s.extract();
    let Observation::Trial { telemetry, .. } = &facts.facts[0].observation else {
        unreachable!()
    };
    assert_eq!(telemetry.as_ref(), Some(want));
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
    s.add(alone("open", 0, -5, &[0]));
    let before = s.extract();
    assert_eq!(before.facts.len(), 1);
    assert_eq!(before.open_trials.len(), 1);
    s.add(end("one", Outcome::Inconclusive, 6));
    let after = s.extract();
    assert_eq!(after.facts, before.facts);
    assert_eq!(after.open_trials, before.open_trials);
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
    s.add(config_loaded(refreshed(&build(1), "9.9.9")));
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
    assert_eq!(facts.shutdown, Some(latest));
    assert_eq!(facts.topology.len(), 1);
    assert_eq!(facts.cores, set(&[0]));
}

#[test]
fn own_pass_takes_the_backend_recorded_before_its_intent() {
    let loaded = |mprime: &str, ycruncher: &str| ConfigLoaded {
        config: config_with_backends(mprime, ycruncher),
        ..config_loaded(build(1))
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

fn ycruncher_workload() -> togi_machine::WorkloadId {
    togi_machine::REGIMES
        .into_iter()
        .find_map(|regime| {
            togi_machine::workloads(regime)
                .iter()
                .find(|w| w.backend == togi_machine::Backend::Ycruncher)
                .map(togi_machine::Workload::id)
        })
        .unwrap()
}

#[test]
fn a_ycruncher_pass_takes_the_ycruncher_identity() {
    let mut s = Script::begin(&[0]);
    s.add(ConfigLoaded {
        config: config_with_backends("/store/m", "/store/y"),
        ..config_loaded(build(1))
    });
    s.add(togi_journal::TrialIntent {
        task_start: None,
        workload: ycruncher_workload(),
        ..alone("y", 0, -3, &[0])
    });
    s.add(pass("y", 3));
    let facts = s.extract();
    let backend = facts.facts[0].source.backend.as_ref().unwrap();
    assert_eq!(backend.backend, togi_machine::Backend::Ycruncher);
    assert_eq!(backend.path, "/store/y");
}

fn context(bios_version: &str) -> SessionContext {
    SessionContext {
        bios_version: bios_version.into(),
        board: "board".into(),
        cpu_model: "cpu".into(),
        microcode: "0".into(),
        boost_limit_mhz: 5000,
    }
}

#[test]
fn each_fact_keeps_the_context_in_force_when_its_trial_opened() {
    let mut s = Script::begin(&[0]);
    s.add(alone("unknown", 0, -5, &[0]));
    s.add(pass("unknown", 5));
    s.add(context("1.2"));
    s.add(alone("before", 0, -10, &[0]));
    s.add(pass("before", 5));
    s.add(alone("spans", 0, -15, &[0]));
    s.add(context("1.3"));
    s.add(pass("spans", 5));
    s.add(alone("after", 0, -20, &[0]));
    s.add(fail("after", Signal::Crash, 5));
    s.add(idle(Some(Condition::Together), Some(profile(&[(0, -20)]))));
    let facts = s.extract();
    let contexts: Vec<_> = facts
        .facts
        .iter()
        .map(|fact| {
            fact.source
                .context
                .as_ref()
                .map(|context| context.bios_version.as_str())
        })
        .collect();
    assert_eq!(
        contexts,
        [None, Some("1.2"), Some("1.2"), Some("1.3"), Some("1.3")]
    );
}
