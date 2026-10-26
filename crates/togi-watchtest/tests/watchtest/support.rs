use togi_journal::{Event, Payload, TrialId, TrialIntent};
use togi_tuner::policy::{Params, Projection, State};

/// The policy state after folding the whole journal.
pub(crate) fn fold(events: &[Event]) -> State {
    let mut state = State::new(Params::SPEC);
    for event in events {
        state.fold(event).expect("a simulated journal folds");
    }
    state
}

/// What status shows of the journal's end.
pub(crate) fn project(events: &[Event]) -> Projection {
    fold(events).project()
}

pub(crate) fn intent_of<'a>(events: &'a [Event], trial: &TrialId) -> &'a TrialIntent {
    events
        .iter()
        .find_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload
                && &intent.trial == trial
            {
                Some(intent)
            } else {
                None
            }
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
