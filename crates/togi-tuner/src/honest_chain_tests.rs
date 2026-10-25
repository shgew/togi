use togi_machine::{CoreSet, PerCore};

use crate::tests::core;
use crate::{HonestChain, PartialBasis, UncoveredReason};

fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

fn requests(pairs: &[(u8, f64)]) -> PerCore<f64> {
    pairs.iter().map(|&(id, volts)| (core(id), volts)).collect()
}

#[test]
fn an_explicit_empty_partial_chain_is_not_coverage() {
    let chain = HonestChain::Partials {
        loaded: set(&[0, 1, 2]),
        basis: PartialBasis::Measured,
        partials: Vec::new(),
    };
    assert!(!chain.is_coverage());
    let wire = serde_json::to_string(&chain).unwrap();
    let decoded: HonestChain = serde_json::from_str(&wire).unwrap();
    assert!(!decoded.is_coverage());
}

#[test]
fn measured_requests_idle_successive_top_groups() {
    let chain = HonestChain::derive(
        set(&[0, 1, 2, 3]),
        &requests(&[(0, 1.19), (1, 1.11), (2, 1.2), (3, 1.09)]),
    );
    assert_eq!(chain.basis(), PartialBasis::Measured);
    assert_eq!(chain.partials(), [set(&[0, 1, 3]), set(&[1, 3])]);
    assert!(chain.is_coverage());
}

#[test]
fn measured_ties_leave_together() {
    let tied = 1.2 - togi_requests::TIE_V / 2.0;
    let chain = HonestChain::derive(
        set(&[0, 1, 2, 3, 4]),
        &requests(&[(0, 1.2), (1, tied), (2, 1.1), (3, 1.0), (4, 0.9)]),
    );
    assert_eq!(chain.partials(), [set(&[2, 3, 4]), set(&[3, 4])]);
}

#[test]
fn measured_chain_never_has_fewer_than_two_loaded_cores() {
    let chain = HonestChain::derive(set(&[0, 1, 2]), &requests(&[(0, 1.3), (1, 1.2), (2, 1.1)]));
    assert_eq!(chain.partials(), [set(&[1, 2])]);
    for partial in chain.partials() {
        assert!(partial.len() >= 2);
    }
}

#[test]
fn measured_top_group_leaving_one_core_is_uncovered() {
    let chain = HonestChain::derive(set(&[0, 1, 2]), &requests(&[(0, 1.2), (1, 1.2), (2, 1.0)]));
    assert_eq!(
        chain,
        HonestChain::Uncovered {
            loaded: set(&[0, 1, 2]),
            basis: PartialBasis::Measured,
            reason: UncoveredReason::TopGroupLeavesTooFew { top: set(&[0, 1]) },
        }
    );
    assert!(chain.partials().is_empty());
    assert!(!chain.is_coverage());
}

#[test]
fn absent_requests_fall_back_to_the_subset_order_not_offset_ties() {
    let chain = HonestChain::derive(set(&[0, 1, 2, 3]), &PerCore::default());
    assert_eq!(
        chain.basis(),
        PartialBasis::SubsetFallback {
            missing: set(&[0, 1, 2, 3])
        }
    );
    assert_eq!(chain.partials(), [set(&[0, 1, 2]), set(&[0, 1])]);
}

#[test]
fn incomplete_requests_are_not_mixed_into_the_fallback() {
    let chain = HonestChain::derive(
        set(&[0, 1, 2, 3]),
        &requests(&[(0, 1.3), (1, 1.2), (3, 1.0)]),
    );
    assert_eq!(
        chain.basis(),
        PartialBasis::SubsetFallback { missing: set(&[2]) }
    );
    assert_eq!(chain.partials(), [set(&[0, 1, 2]), set(&[0, 1])]);
}

#[test]
fn a_non_finite_request_counts_as_missing() {
    let chain = HonestChain::derive(
        set(&[0, 1, 2]),
        &requests(&[(0, 1.3), (1, f64::NAN), (2, 1.0)]),
    );
    assert_eq!(
        chain.basis(),
        PartialBasis::SubsetFallback { missing: set(&[1]) }
    );
}

#[test]
fn requests_of_unloaded_cores_are_ignored() {
    let chain = HonestChain::derive(
        set(&[0, 1, 2]),
        &requests(&[(0, 1.3), (1, 1.2), (2, 1.1), (5, 1.4)]),
    );
    assert_eq!(chain.basis(), PartialBasis::Measured);
    assert_eq!(chain.partials(), [set(&[1, 2])]);
}

#[test]
fn an_empty_or_small_load_is_uncovered_not_coverage() {
    for loaded in [set(&[]), set(&[4]), set(&[4, 5])] {
        let chain = HonestChain::derive(loaded, &PerCore::default());
        assert!(matches!(
            chain,
            HonestChain::Uncovered {
                reason: UncoveredReason::TooFewLoaded,
                ..
            }
        ));
        assert!(!chain.is_coverage());
        assert!(chain.partials().is_empty());
        assert_eq!(chain.loaded(), loaded);
    }
}

#[test]
fn the_chain_serializes_self_describing() {
    let chain = HonestChain::derive(set(&[0, 1]), &PerCore::default());
    assert_eq!(
        serde_json::to_value(&chain).unwrap(),
        serde_json::json!({
            "coverage": "uncovered",
            "loaded": [0, 1],
            "basis": {"basis": "subset_fallback", "missing": [0, 1]},
            "reason": {"reason": "too_few_loaded"},
        })
    );
}
