use std::num::NonZeroU32;

use test_case::test_case;
use togi_machine::Regime;
use togi_tuner::Secs;
use togi_tuner::night::{
    Conclusion, Credit, Launch, PlacementValidity, adequately_exposed, warm_up_secs,
};

fn launch(regime: Regime, intended: u32, ran: u32) -> Launch {
    Launch {
        regime,
        intended: Secs::new(NonZeroU32::new(intended).unwrap()),
        ran,
        conclusion: Conclusion::Conclusive,
        placement: PlacementValidity::Valid,
        credit: Credit::Granted,
    }
}

#[test_case(Regime::R1, 5)]
#[test_case(Regime::R7, 5)]
#[test_case(Regime::R8, 30)]
#[test_case(Regime::R9, 30)]
fn the_warm_up_boundary_is_per_regime(regime: Regime, secs: u32) {
    assert_eq!(warm_up_secs(regime), secs);
}

#[test]
fn one_complete_conclusive_valid_credited_launch_is_adequate() {
    assert!(adequately_exposed(&[launch(Regime::R1, 90, 90)]));
}

#[test]
fn many_truncated_launches_never_add_up_to_a_complete_one() {
    let truncated = vec![launch(Regime::R1, 90, 89); 50];
    assert!(!adequately_exposed(&truncated));
}

#[test_case(Conclusion::Inconclusive, PlacementValidity::Valid, Credit::Granted; "inconclusive")]
#[test_case(Conclusion::Conclusive, PlacementValidity::Withheld, Credit::Granted; "placement withheld")]
#[test_case(Conclusion::Conclusive, PlacementValidity::Valid, Credit::Withheld; "host without credit")]
fn a_launch_without_conclusion_placement_or_credit_is_not_adequate(
    conclusion: Conclusion,
    placement: PlacementValidity,
    credit: Credit,
) {
    let launch = Launch {
        conclusion,
        placement,
        credit,
        ..launch(Regime::R2, 90, 90)
    };
    assert!(!adequately_exposed(&[launch]));
}

#[test]
fn a_duration_inside_the_warm_up_has_no_eligible_seconds() {
    let short = launch(Regime::R8, 30, 30);
    assert!(!short.completed());
    assert_eq!(short.eligible_seconds(), 0);
    assert!(!adequately_exposed(&[short]));
}

#[test]
fn one_complete_launch_among_failures_and_partials_suffices() {
    let launches = [launch(Regime::R3, 120, 10), launch(Regime::R3, 120, 120)];
    assert!(adequately_exposed(&launches));
    assert_eq!(launches[1].eligible_seconds(), 115);
}
