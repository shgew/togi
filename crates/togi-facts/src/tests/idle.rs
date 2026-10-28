use test_case::test_case;

use togi_journal::{Attribution, TrialId};
use togi_machine::{BootId, Condition, Regime, Signal};

use super::{Script, build, config_loaded, first, idle, profile, refreshed, set};
use crate::Observation;

#[test_case(Some(Condition::Together), Attribution::Unattributed, None, false, Some(&[(0, -10), (1, -20)]), true; "together")]
#[test_case(Some(Condition::Parked), Attribution::Unattributed, None, false, Some(&[(0, -10), (1, 0)]), true; "parked")]
#[test_case(Some(Condition::Together), Attribution::Attributed, None, false, Some(&[(0, -10), (1, -20)]), false; "attributed")]
#[test_case(Some(Condition::Alone), Attribution::Unattributed, None, false, Some(&[(0, -10), (1, -20)]), false; "alone")]
#[test_case(None, Attribution::Unattributed, None, false, Some(&[(0, -10), (1, -20)]), false; "no condition")]
#[test_case(None, Attribution::Unattributed, None, false, None, false; "stray")]
#[test_case(Some(Condition::Together), Attribution::Unattributed, None, false, None, false; "no profile")]
#[test_case(Some(Condition::Together), Attribution::Unattributed, None, false, Some(&[(0, -10)]), false; "partial profile")]
#[test_case(Some(Condition::Together), Attribution::Unattributed, None, false, Some(&[(0, -10), (2, -10)]), false; "foreign core")]
#[test_case(Some(Condition::Together), Attribution::Unattributed, Some("one"), false, Some(&[(0, -10), (1, -20)]), false; "trial failure is not idle")]
#[test_case(Some(Condition::Together), Attribution::Unattributed, None, true, Some(&[(0, -10), (1, -20)]), false; "known failure")]
fn idle_failures(
    condition: Option<Condition>,
    attribution: Attribution,
    trial: Option<&str>,
    known_failure: bool,
    offsets: Option<&[(i64, i64)]>,
    want: bool,
) {
    let mut s = Script::begin(&[1, 0]);
    let original = s.add(idle(
        Some(Condition::Together),
        Some(profile(&[(0, -1), (1, -1)])),
    ));
    let mut failure = idle(condition, offsets.map(profile));
    failure.attribution = attribution;
    failure.trial = trial.map(|id| TrialId::from(id.to_owned()));
    failure.known_failure = known_failure.then_some(original);
    let seq = s.add(failure);
    let failure_condition = condition;
    let facts = s.extract();
    let ours: Vec<_> = facts.facts.iter().filter(|f| f.source.seq == seq).collect();
    assert_eq!(ours.len(), usize::from(want));
    if let [fact] = ours[..] {
        assert_eq!(fact.source.time, Script::time(seq));
        assert_eq!(fact.source.session, first());
        assert_eq!(fact.source.epoch, 1);
        assert_eq!(fact.source.trial, None);
        assert_eq!(fact.loaded(), set(&[0, 1]));
        assert_eq!(fact.regime(), Regime::R6);
        let Observation::IdleFailure {
            signal, condition, ..
        } = &fact.observation
        else {
            unreachable!()
        };
        assert_eq!(*signal, Signal::Crash);
        assert_eq!(Some(*condition), failure_condition);
        assert_eq!(Some(fact.profile()), offsets.map(profile).as_ref());
    }
}

#[test_case(Regime::R1; "r1")]
#[test_case(Regime::R3; "r3")]
#[test_case(Regime::R7; "r7")]
fn idle_class_wildcard_independent_of_diagnostic_regime(regime: Regime) {
    let mut s = Script::begin(&[0, 1]);
    let mut failure = idle(Some(Condition::Parked), Some(profile(&[(0, -5), (1, -6)])));
    failure.regime = Some(regime);
    s.add(failure);
    let facts = s.extract();
    let fact = &facts.facts[0];
    assert_eq!(fact.regime(), Regime::R6);
    assert_eq!(fact.loaded(), set(&[0, 1]));
    let Observation::IdleFailure {
        regime: diagnostic, ..
    } = &fact.observation
    else {
        unreachable!()
    };
    assert_eq!(*diagnostic, Some(regime));
}

#[test_case(Condition::Together; "together")]
#[test_case(Condition::Parked; "parked")]
fn known_failure_skip_does_not_create_idle_fact(condition: Condition) {
    let mut s = Script::begin(&[0, 1]);
    let original = s.add(idle(Some(condition), Some(profile(&[(0, -9), (1, -10)]))));
    let mut skip = idle(Some(condition), Some(profile(&[(0, -9), (1, -10)])));
    skip.known_failure = Some(original);
    s.add_caused(&[original], skip);
    let facts = s.extract();
    assert_eq!(facts.facts.len(), 1);
    assert_eq!(facts.facts[0].source.seq, original);
}

#[test]
fn idle_fact_takes_the_refreshed_build_the_failures_boot_and_the_start_epoch() {
    let refreshed_build = refreshed(&build(1), "0.2.0");
    let mut s = Script::begin(&[0, 1]);
    s.add(config_loaded(refreshed_build.clone()));
    s.boot("b");
    let seq = s.add(idle(
        Some(Condition::Together),
        Some(profile(&[(0, -1), (1, -1)])),
    ));
    let facts = s.extract();
    let [fact] = &facts.facts[..] else {
        unreachable!()
    };
    assert_eq!(fact.source.seq, seq);
    assert_eq!(fact.source.build, refreshed_build);
    assert_eq!(fact.source.boot, BootId::from("b".to_owned()));
    assert_eq!(fact.source.epoch, 1);
    assert_eq!(
        facts.build,
        build(1),
        "the session's stamp stays the start's"
    );
}
