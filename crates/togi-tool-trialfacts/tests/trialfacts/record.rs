use togi_facts::TrialClass;
use togi_machine::{Regime, TargetOffset};
use togi_tool_trialfacts::{Class, InvalidProfile, Profile};

use crate::builders::{offsets, per_core, set, workload};

#[test]
fn a_class_names_what_ran() {
    let trial = Class::Trial(TrialClass {
        workload: workload(Regime::R7, 0),
        loaded: set(&[0, 1]),
        duration_s: 120,
    });
    assert_eq!(trial.regime(), Regime::R7);
    assert_eq!(trial.workload(), Some(workload(Regime::R7, 0)));
    assert_eq!(trial.loaded(), set(&[0, 1]));
    assert_eq!(trial.duration_s(), 120);
    let idle = Class::Idle { loaded: set(&[0]) };
    assert_eq!(idle.regime(), Regime::R6);
    assert_eq!(idle.workload(), None);
    assert_eq!(idle.duration_s(), 0);
}

#[test]
fn a_profile_holds_one_to_sixteen_offsets() {
    assert_eq!(Profile::new(&[]), Err(InvalidProfile::Empty));
    assert_eq!(
        Profile::new(&[TargetOffset::ZERO; 17]),
        Err(InvalidProfile::TooLong(17))
    );
    let sixteen = vec![TargetOffset::ZERO; 16];
    assert_eq!(Profile::new(&sixteen).unwrap().as_slice(), sixteen);
    assert_eq!(Profile::new(&sixteen).unwrap().core_count(), 16);
    assert_eq!(Profile::new(&[TargetOffset::ZERO]).unwrap().core_count(), 1);
    assert_eq!(
        Profile::new(&offsets(&[-10, 0])).unwrap().as_slice(),
        offsets(&[-10, 0])
    );
}

#[test]
fn a_profile_of_a_dense_per_core_value_keeps_core_order() {
    let profile = Profile::from_per_core(&per_core(&[-5, -10, 0])).unwrap();
    assert_eq!(profile.as_slice(), offsets(&[-5, -10, 0]));
}

#[test]
fn a_per_core_value_with_a_hole_is_refused() {
    let sparse = [(0, -10), (2, 0)]
        .into_iter()
        .map(|(n, v)| (crate::builders::core(n), TargetOffset::new(v).unwrap()))
        .collect();
    assert_eq!(
        Profile::from_per_core(&sparse),
        Err(InvalidProfile::Sparse(1))
    );
    assert_eq!(
        Profile::from_per_core(&togi_machine::PerCore::default()),
        Err(InvalidProfile::Empty)
    );
}
