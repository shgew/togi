use togi_journal::{Attribution, Outcome};
use togi_machine::Condition;

use super::{Source, context, day, summary_of, trial_end};

fn source_with_inconclusive(count: usize) -> Source {
    let mut source = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
    source.pass(0, -30, Condition::Alone);
    source.fail(1, -30, Condition::Alone, Attribution::Attributed);
    for _ in 0..count {
        let trial = source.intent(0, -31, Condition::Alone);
        source.add(trial_end(&trial, Outcome::Inconclusive, None, None));
    }
    source
}

#[test]
fn cost_selection_independent_of_event_volume() {
    let small = source_with_inconclusive(10);
    let large = source_with_inconclusive(10_000);
    assert!(large.events.len() > 20_000);

    let small = summary_of(&small).unwrap();
    let large = summary_of(&large).unwrap();
    assert_eq!(small.retained_seeds(), large.retained_seeds());
    assert_eq!(small.facts().facts.len(), large.facts().facts.len());
    assert!(large.facts().open_trials.is_empty());
    assert_eq!(
        small, large,
        "inconclusive trials leave no trace in the summary"
    );
}
