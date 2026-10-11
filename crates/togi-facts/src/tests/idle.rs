use togi_journal::{Attribution, Payload, TrialId};
use togi_machine::{Condition, Regime, Signal};

use super::{Script, first, idle, profile, set};
use crate::Observation;

#[test]
fn idle_failures() {
    struct Case {
        name: &'static str,
        condition: Option<Condition>,
        attribution: Attribution,
        trial: Option<&'static str>,
        known_failure: bool,
        profile: Option<&'static [(i64, i64)]>,
        want: bool,
    }
    let base = Case {
        name: "",
        condition: Some(Condition::Together),
        attribution: Attribution::Unattributed,
        trial: None,
        known_failure: false,
        profile: Some(&[(0, -10), (1, -20)]),
        want: true,
    };
    let cases = [
        Case {
            name: "together",
            ..base
        },
        Case {
            name: "parked",
            condition: Some(Condition::Parked),
            profile: Some(&[(0, -10), (1, 0)]),
            ..base
        },
        Case {
            name: "attributed",
            attribution: Attribution::Attributed,
            want: false,
            ..base
        },
        Case {
            name: "alone",
            condition: Some(Condition::Alone),
            want: false,
            ..base
        },
        Case {
            name: "stray",
            condition: None,
            profile: None,
            want: false,
            ..base
        },
        Case {
            name: "no profile",
            profile: None,
            want: false,
            ..base
        },
        Case {
            name: "partial profile",
            profile: Some(&[(0, -10)]),
            want: false,
            ..base
        },
        Case {
            name: "foreign core",
            profile: Some(&[(0, -10), (2, -10)]),
            want: false,
            ..base
        },
        Case {
            name: "trial failure is not idle",
            trial: Some("one"),
            want: false,
            ..base
        },
        Case {
            name: "known failure",
            known_failure: true,
            want: false,
            ..base
        },
    ];
    for case in cases {
        let mut s = Script::begin(&[1, 0]);
        let original = s.add(idle(
            Some(Condition::Together),
            Some(profile(&[(0, -1), (1, -1)])),
        ));
        let mut failure = idle(case.condition, case.profile.map(profile));
        failure.attribution = case.attribution;
        failure.trial = case.trial.map(|id| TrialId::from(id.to_owned()));
        failure.known_failure = case.known_failure.then_some(original);
        let seq = s.add(failure);
        let facts = s.extract();
        let ours: Vec<_> = facts.facts.iter().filter(|f| f.source.seq == seq).collect();
        assert_eq!(ours.len(), usize::from(case.want), "{}", case.name);
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
            assert_eq!(Some(*condition), case.condition);
            assert_eq!(Some(fact.profile()), case.profile.map(profile).as_ref());
        }
    }
}

#[test]
fn idle_class_wildcard_independent_of_diagnostic_regime() {
    for regime in [Regime::R1, Regime::R3, Regime::R7] {
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
        let Payload::FailureCarried(carried) = fact.to_payload() else {
            unreachable!()
        };
        assert_eq!(carried.class.regime, Regime::R6);
        assert_eq!(carried.class.workload, None);
        assert_eq!(carried.class.cores, set(&[0, 1]));
        assert_eq!(carried.class.duration_s, 0);
    }
}

#[test]
fn known_failure_skip_does_not_create_idle_fact() {
    for condition in [Condition::Together, Condition::Parked] {
        let mut s = Script::begin(&[0, 1]);
        let original = s.add(idle(Some(condition), Some(profile(&[(0, -9), (1, -10)]))));
        let mut skip = idle(Some(condition), Some(profile(&[(0, -9), (1, -10)])));
        skip.known_failure = Some(original);
        s.add_caused(&[original], skip);
        let facts = s.extract();
        assert_eq!(facts.facts.len(), 1);
        assert_eq!(facts.facts[0].source.seq, original);
    }
}
