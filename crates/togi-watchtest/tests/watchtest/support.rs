use togi_journal::{Event, Payload, TrialIntent};
use togi_tuner::State;

pub(crate) fn fold(events: &[Event]) -> State {
    togi_tuner::replay(events).expect("a simulated journal folds")
}

/// The tuner state after each prefix of `events` whose length is in `lengths`, from one fold of the journal.
pub(crate) fn states_at(events: &[Event], lengths: &[usize]) -> Vec<State> {
    let mut state = State::new();
    let mut found = Vec::new();
    for (at, event) in events.iter().enumerate() {
        state.fold(event).expect("a simulated journal folds");
        if lengths.contains(&(at + 1)) {
            found.push((at + 1, state.clone()));
        }
    }
    lengths
        .iter()
        .map(|length| {
            found
                .iter()
                .find(|(at, _)| at == length)
                .map(|(_, state)| state.clone())
                .expect("the journal is as long as the cut")
        })
        .collect()
}

pub(crate) fn intent_of<'a>(events: &'a [Event], trial: &togi_journal::TrialId) -> &'a TrialIntent {
    events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::TrialIntent(intent) if &intent.trial == trial => Some(intent),
            _ => None,
        })
        .expect("a trial.start follows its intent")
}

/// The intent of the trial a journal ends on the start of, with no end recorded after it.
pub(crate) fn running_trial(events: &[Event]) -> &TrialIntent {
    let last = events.last().expect("a journal has events");
    let Payload::TrialStart(start) = &last.payload else {
        panic!("the journal ends on {:?}, not a trial.start", last.kind());
    };
    assert!(
        !events.iter().any(
            |event| matches!(&event.payload, Payload::TrialEnd(end) if end.trial == start.trial)
        ),
        "trial {} already ended",
        start.trial.as_str()
    );
    intent_of(events, &start.trial)
}

pub(crate) fn lines(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|event| event.encode().expect("an event encodes"))
        .collect()
}
