use test_case::test_case;

use std::collections::BTreeMap;

use togi_journal::{
    Attribution, BackendIdentity, FactSource, FailureCarried, Outcome, Seq, TrialCarried,
};
use togi_machine::{Backend, BootId, Condition, Regime, Signal};

use super::{Script, build, core, idle, pass, profile, session, set, trial, workload};
use crate::{FactsError, Observation};

/// A historical archive's source: an observation of another session.
fn source() -> FactSource {
    FactSource {
        session: session("20260901T000000Z"),
        seq: Seq::new(40).unwrap(),
        build: build(4),
        trial: Some(trial("0304")),
        time: Script::time(Seq::new(40).unwrap()),
        boot: BootId::from("source-boot".to_owned()),
        backend: Some(BackendIdentity {
            backend: Backend::Mprime,
            path: "/store/mprime".into(),
        }),
    }
}

/// A well-formed `trial.carried` copy of a failed R7 trial on cores 3 and 7.
fn trial_copy() -> TrialCarried {
    TrialCarried {
        source: source(),
        class: togi_journal::TrialClass {
            regime: Regime::R7,
            workload: Some(workload(Regime::R7)),
            cores: set(&[3, 7]),
            duration_s: 120,
        },
        condition: Condition::Together,
        activity: None,
        rerun: false,
        profile: profile(&[(1, -20), (3, -30), (7, -40)]),
        outcome: Outcome::Failure,
        signal: Some(Signal::ComputationError),
        duration_s: 11,
        core: Some(core(7)),
        stalled_core: None,
        voltage_requests_v: togi_machine::PerCore::default(),
        top_requesters: set(&[]),
        ccd_mhz: BTreeMap::new(),
    }
}

/// A well-formed `failure.carried` copy of an idle failure with all three cores profiled.
fn idle_copy() -> FailureCarried {
    FailureCarried {
        source: FactSource {
            trial: None,
            backend: None,
            ..source()
        },
        class: togi_journal::TrialClass {
            regime: Regime::R6,
            workload: None,
            cores: set(&[1, 3, 7]),
            duration_s: 0,
        },
        failure: idle(
            Some(Condition::Parked),
            Some(profile(&[(1, -20), (3, -30), (7, -40)])),
        ),
    }
}

#[test]
fn well_formed_copies_are_carried_not_own_facts() {
    let mut s = Script::begin(&[7, 1, 3]);
    let trial_seq = s.add(trial_copy());
    let idle_seq = s.add(idle_copy());
    let facts = s.extract();
    assert!(facts.facts.is_empty(), "copies are not own facts");
    let [trial_fact, idle_fact] = &facts.carried[..] else {
        unreachable!()
    };
    assert_eq!(trial_fact.local, trial_seq);
    assert_eq!(trial_fact.source.session, source().session);
    assert_eq!(trial_fact.source.seq, source().seq);
    assert_eq!(trial_fact.source.epoch, 4);
    assert_eq!(trial_fact.source.backend, source().backend);
    assert!(trial_fact.is_failure());
    assert_eq!(trial_fact.loaded(), set(&[3, 7]));
    assert_eq!(idle_fact.local, idle_seq);
    assert!(matches!(
        idle_fact.observation,
        Observation::IdleFailure {
            condition: Condition::Parked,
            ..
        }
    ));
    assert_eq!(idle_fact.loaded(), set(&[1, 3, 7]));
}

#[test_case(|copy| copy.outcome = Outcome::Inconclusive, |error| matches!(error, FactsError::NotDecisive { .. }); "inconclusive")]
#[test_case(|copy| copy.signal = None, |error| matches!(error, FactsError::Incomplete { .. }); "signal")]
#[test_case(|copy| copy.class.workload = None, |error| matches!(error, FactsError::Incomplete { .. }); "workload")]
fn carried_trials_must_be_decisive_well_formed(
    change: fn(&mut TrialCarried),
    expected: fn(&FactsError<std::convert::Infallible>) -> bool,
) {
    let mut copy = trial_copy();
    change(&mut copy);
    let mut s = Script::begin(&[7, 1, 3]);
    s.add(copy);
    let result = s.try_extract(super::first());
    assert!(
        matches!(&result, Err(error) if expected(error)),
        "{result:?}"
    );
}

#[test_case(|copy| copy.failure.trial = Some(trial("x")), |error| matches!(error, FactsError::NotIdle { .. }); "in a trial")]
#[test_case(|copy| copy.failure.attribution = Attribution::Attributed, |error| matches!(error, FactsError::NotIdle { .. }); "attributed")]
#[test_case(|copy| copy.failure.known_failure = Some(Seq::new(1).unwrap()), |error| matches!(error, FactsError::NotIdle { .. }); "known failure")]
#[test_case(|copy| copy.failure.condition = Some(Condition::Alone), |error| matches!(error, FactsError::NotIdle { .. }); "alone")]
#[test_case(|copy| copy.failure.profile = Some(profile(&[])), |error| matches!(error, FactsError::NotIdle { .. }); "empty profile")]
#[test_case(|copy| copy.class.cores = set(&[1, 3]), |error| matches!(error, FactsError::NotIdle { .. }); "profile cores differ from class")]
#[test_case(|copy| copy.failure.condition = None, |error| matches!(error, FactsError::Incomplete { .. }); "condition")]
#[test_case(|copy| copy.failure.profile = None, |error| matches!(error, FactsError::Incomplete { .. }); "profile")]
fn carried_idle_failures_must_be_well_formed(
    change: fn(&mut FailureCarried),
    expected: fn(&FactsError<std::convert::Infallible>) -> bool,
) {
    let mut copy = idle_copy();
    change(&mut copy);
    let mut s = Script::begin(&[7, 1, 3]);
    s.add(copy);
    let result = s.try_extract(super::first());
    assert!(
        matches!(&result, Err(error) if expected(error)),
        "{result:?}"
    );
}

#[test]
fn accessors_agree_with_the_observation() {
    let mut s = Script::begin(&[0, 1]);
    s.add(super::alone("p", 1, -7, &[0, 1]));
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
}
