use togi_machine::Regime;

use super::{missing_coverage, rounds};

#[test]
fn a_full_cycle_lacks_nothing() {
    let steps = [
        Regime::R1,
        Regime::R1,
        Regime::R1,
        Regime::R2,
        Regime::R2,
        Regime::R2,
        Regime::R3,
        Regime::R4,
        Regime::R5,
        Regime::R6,
        Regime::R7,
        Regime::R7,
        Regime::R7,
    ];
    assert!(missing_coverage(&steps).is_empty());
}

#[test]
fn a_short_cycle_names_each_regime_it_lacks() {
    let steps = [Regime::R1, Regime::R2, Regime::R2, Regime::R2, Regime::R3];
    assert_eq!(
        missing_coverage(&steps),
        [
            "R1: 2 more steps",
            "R4: 1 more steps",
            "R5: 1 more steps",
            "R6: 1 more steps",
            "R7: 3 more steps",
        ]
    );
}

#[test]
fn rounds_agree_with_their_count() {
    assert_eq!(rounds(1), "1 round");
    assert_eq!(rounds(2), "2 rounds");
    assert_eq!(rounds(3), "3 rounds");
}
