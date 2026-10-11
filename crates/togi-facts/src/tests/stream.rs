use togi_journal::{Event, Outcome, Payload, Seq, Stamp};
use togi_machine::BootId;

use super::{Script, alone, end, first, start};
use crate::FactsError;
use crate::extract::Extractor;

fn event(seq: u64, payload: impl Into<Payload>) -> Event {
    let seq = Seq::new(seq).unwrap();
    Event::new(
        seq,
        Stamp {
            time: Script::time(seq),
            mono: None,
        },
        BootId::from("a".to_owned()),
        Vec::new(),
        payload.into(),
    )
}

#[test]
fn extract_streaming_retains_no_events() {
    const TRIALS: u64 = 10_000;
    let events =
        std::iter::once(event(1, start(first(), &[0], 1))).chain((0..TRIALS).flat_map(|i| {
            let id = format!("t{i}");
            [
                event(2 + 2 * i, alone(&id, 0, -10, &[0])),
                event(3 + 2 * i, end(&id, Outcome::Inconclusive, 1)),
            ]
        }));
    let mut events = events;
    let first_event = events.next().unwrap();
    let mut extractor =
        Extractor::start::<std::convert::Infallible>(first(), &first_event).unwrap();
    let mut peak = 0;
    for event in events {
        extractor.fold::<std::convert::Infallible>(&event).unwrap();
        peak = peak.max(extractor.retained());
    }
    assert!(peak <= 2, "state grew with the stream: {peak}");
    assert_eq!(extractor.retained(), 0);
    let facts = extractor.finish();
    assert!(facts.facts.is_empty());
    assert!(facts.open_trials.is_empty());
}

#[test]
fn retained_state_grows_with_facts_only() {
    const TRIALS: u64 = 500;
    let mut extractor = None;
    for event in
        std::iter::once(event(1, start(first(), &[0], 1))).chain((0..TRIALS).flat_map(|i| {
            let id = format!("t{i}");
            [
                event(2 + 2 * i, alone(&id, 0, -10, &[0])),
                event(3 + 2 * i, super::pass(&id, 1)),
            ]
        }))
    {
        match &mut extractor {
            None => {
                extractor =
                    Some(Extractor::start::<std::convert::Infallible>(first(), &event).unwrap());
            }
            Some(extractor) => extractor.fold::<std::convert::Infallible>(&event).unwrap(),
        }
    }
    let facts = extractor.unwrap().finish();
    assert_eq!(facts.facts.len(), usize::try_from(TRIALS).unwrap());
}

#[test]
fn extract_propagates_read_error_no_partial_result() {
    let events: Vec<Result<Event, &'static str>> = vec![
        Ok(event(1, start(first(), &[0], 1))),
        Ok(event(2, alone("one", 0, -10, &[0]))),
        Err("broken line"),
        Ok(event(3, super::pass("one", 1))),
    ];
    let result = crate::extract(first(), events);
    assert!(matches!(result, Err(FactsError::Read("broken line"))));
}
