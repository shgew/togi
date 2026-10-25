use togi_journal::{Activity, Outcome};
use togi_machine::Regime;

use super::support::{Log, intent, trial};
use crate::project::Projection;
use crate::report::compute;

#[test]
fn the_failure_rate_by_depth_lists_multi_core_classes_that_failed() {
    let mut log = Log::new(&[(0, 0), (1, 0), (2, 1)]);
    let planned = |id: &str, cores: &[i64], offsets: &[i64]| {
        let mut planned = intent(id, Regime::R7, cores, offsets);
        planned.activity = Some(Activity::Checking);
        planned
    };
    trial(
        &mut log,
        planned("a", &[0, 1], &[-10, -20, 0]),
        Outcome::Pass,
    );
    trial(
        &mut log,
        planned("b", &[0, 1], &[-10, -20, 0]),
        Outcome::Failure,
    );
    trial(
        &mut log,
        planned("c", &[0, 1], &[-5, -20, 0]),
        Outcome::Pass,
    );
    trial(&mut log, planned("d", &[0, 2], &[-5, 0, -7]), Outcome::Pass);
    trial(&mut log, planned("e", &[0], &[-5, 0, 0]), Outcome::Failure);
    let journal = log.journal();
    let rates = compute(&Projection::new(&journal), None).depth;
    let labels: Vec<_> = rates.iter().map(|r| r.class.label()).collect();
    let got: Vec<_> = rates
        .iter()
        .zip(&labels)
        .map(|(r, label)| (label.as_str(), r.depth, r.trials, r.failures))
        .collect();
    assert_eq!(
        got,
        [
            ("R7/mprime-avx2-36k-248k-allcore/00,01/120s", -10, 2, 1),
            ("R7/mprime-avx2-36k-248k-allcore/00,01/120s", -5, 1, 0),
        ]
    );
}
