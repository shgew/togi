use togi_machine::{Regime, TargetOffset};
use togi_tool_trialfacts::{Class, InvalidProfile, Profile, TrialClass};

use crate::builders::{offsets, set, workload};

#[test]
fn a_class_names_what_ran() {
    let trial = Class::Trial(TrialClass {
        workload: workload(Regime::R7, 0),
        loaded: set(&[0, 1]),
        duration_s: 120,
    });
    assert_eq!(trial.regime(), Regime::R7);
    let idle = Class::Idle { loaded: set(&[0]) };
    assert_eq!(idle.regime(), Regime::R6);
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
    assert_eq!(
        Profile::new(&[TargetOffset::ZERO]).unwrap().as_slice(),
        [TargetOffset::ZERO]
    );
    assert_eq!(
        Profile::new(&offsets(&[-10, 0])).unwrap().as_slice(),
        offsets(&[-10, 0])
    );
}
