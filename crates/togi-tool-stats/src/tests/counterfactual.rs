use test_case::test_case;
use togi_machine::PerCore;
use togi_tuner::ChainOrder;

use super::support::{core, set};
use crate::counterfactual::{next_set, within_tie};

/// The order of a CCD whose cores 0 and 1 request one count apart, core 2 two counts below and core 3 far below.
fn order() -> ChainOrder {
    ChainOrder {
        groups: vec![set(&[0]), set(&[1]), set(&[2]), set(&[3])],
        values: [(0, 1.2), (1, 1.197), (2, 1.1928), (3, 1.1)]
            .into_iter()
            .map(|(n, value)| (core(n), value))
            .collect::<PerCore<f64>>(),
        proxy: set(&[]),
        sources: Vec::new(),
    }
}

#[test_case(&[1, 2, 3], true ; "idling the top core is the tuner's own order")]
#[test_case(&[0, 2, 3], true ; "idling a core one count below the top is a tie")]
#[test_case(&[2, 3], true ; "idling both tied cores is a tie")]
#[test_case(&[0, 1, 3], false ; "a core two counts below the top is no tie")]
#[test_case(&[0, 1, 2], false ; "a core far below the top is no tie")]
#[test_case(&[0, 1, 2, 3], false ; "a set that idles nothing")]
#[test_case(&[], false ; "an empty set")]
#[test_case(&[1, 2, 4], false ; "a set outside the load")]
fn a_held_set_ties_when_every_core_it_idles_requests_within_one_count_of_the_top(
    held: &[i64],
    tie: bool,
) {
    assert_eq!(within_tie(set(&[0, 1, 2, 3]), set(held), &order()), tie);
}

#[test_case(&[0, 1, 2, 3], &[1, 2, 3] ; "the top group idles")]
#[test_case(&[1, 2, 3], &[1, 2, 3] ; "a load without the top group keeps its cores")]
#[test_case(&[0, 3], &[] ; "fewer than two remaining cores end the chain")]
fn the_next_set_idles_the_orders_top_group(load: &[i64], next: &[i64]) {
    assert_eq!(next_set(set(load), &order()), set(next));
}
