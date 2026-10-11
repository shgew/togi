use std::collections::HashSet;

use togi_facts::{Decisive, TrialClass};
use togi_machine::{Condition, Regime};
use togi_tool_trialfacts::{Class, ProfileClassKey};

use crate::builders::{crash, fact, set, workload};

const FIRST: &str = "20260101T000000Z";
const SECOND: &str = "20260102T000000Z";

#[test]
fn the_key_ignores_provenance_condition_and_outcome() {
    let base = fact(FIRST, 1, &[-10, 0], Decisive::Pass);
    let mut same = fact(SECOND, 9, &[-10, 0], crash(Some(0)));
    same.condition = Condition::Together;
    same.duration_s = 12;
    let mut other_profile = base.clone();
    other_profile.profile = fact(FIRST, 1, &[-11, 0], Decisive::Pass).profile;
    let mut other_duration = base.clone();
    other_duration.class = Class::Trial(TrialClass {
        workload: workload(Regime::R1, 0),
        loaded: set(&[0]),
        duration_s: 120,
    });
    let key = ProfileClassKey::from(&base);
    assert_eq!(key, ProfileClassKey::from(&same));
    assert_ne!(key, ProfileClassKey::from(&other_profile));
    assert_ne!(key, ProfileClassKey::from(&other_duration));
}

#[test]
fn the_loaded_cores_are_a_set_and_the_key_hashes() {
    let mut forward = fact(FIRST, 1, &[-10, 0], Decisive::Pass);
    let mut backward = forward.clone();
    forward.class = Class::Trial(TrialClass {
        workload: workload(Regime::R2, 0),
        loaded: set(&[0, 1]),
        duration_s: 90,
    });
    backward.class = Class::Trial(TrialClass {
        workload: workload(Regime::R2, 0),
        loaded: set(&[1, 0]),
        duration_s: 90,
    });
    let keys: HashSet<_> = [&forward, &backward]
        .into_iter()
        .map(ProfileClassKey::from)
        .collect();
    assert_eq!(keys.len(), 1);
}

#[test]
fn an_idle_failure_is_not_a_trial_of_the_same_cores() {
    let trial = fact(FIRST, 1, &[-10, 0], Decisive::Pass);
    let mut idle = trial.clone();
    idle.class = Class::Idle { loaded: set(&[0]) };
    assert_ne!(ProfileClassKey::from(&trial), ProfileClassKey::from(&idle));
}
