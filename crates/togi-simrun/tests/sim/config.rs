use togi_config::Config;
use togi_journal::{Decision, Event, EventKind, Payload, Seq, TrialIntent};
use togi_machine::CoreId;
use togi_simrun::Observe;

use crate::common::{Bench, finish, hunt_machine};

fn recorded() -> Config {
    Config::parse(
        r#"{
            "durations": {"search_trial_s": 17},
            "backends": {
                "mprime": "/nix/store/recorded-mprime",
                "ycruncher": "/nix/store/recorded-ycruncher"
            }
        }"#,
    )
    .unwrap()
}

fn nth(n: usize, kind: EventKind) -> togi_simrun::Observer {
    let mut seen = 0;
    Box::new(move |event| {
        if event.kind() == kind {
            seen += 1;
        }
        if seen == n {
            Observe::Stop
        } else {
            Observe::Continue
        }
    })
}

fn intents(events: &[Event]) -> Vec<&TrialIntent> {
    events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some(intent)
            } else {
                None
            }
        })
        .collect()
}

fn run_until(bench: &Bench, config: Config, observer: togi_simrun::Observer) {
    let mut input = bench.input();
    input.config = config;
    input.observer = Some(observer);
    finish(input);
}

#[test]
fn a_journal_without_a_recorded_configuration_gives_the_fresh_one() {
    let bench = Bench::new(&hunt_machine(2), 1);
    let fresh = Config::parse(r#"{"durations": {"search_trial_s": 5}}"#).unwrap();
    assert_eq!(bench.recorded_config(fresh.clone()), fresh);
}

#[test]
fn a_resume_under_the_recorded_configuration_repeats_the_recorded_trials() {
    let definition = hunt_machine(2);
    let recorded = recorded();

    let reference = Bench::new(&definition, 1);
    run_until(&reference, recorded.clone(), nth(3, EventKind::TrialIntent));
    let reference_events = reference.events();
    let want = intents(&reference_events)[2].clone();

    let stopped = Bench::new(&definition, 1);
    run_until(&stopped, recorded.clone(), nth(2, EventKind::TrialEnd));
    let stopped_events = stopped.events();
    let done = intents(&stopped_events).len();
    assert!(done >= 2);
    assert!(
        intents(&stopped_events)
            .iter()
            .all(|intent| intent.duration_s == 17)
    );

    let got = stopped.recorded_config(Config::default());
    assert_eq!(got, recorded);

    let next = |config: Config| {
        let fork = stopped.fork(&definition, 1);
        run_until(&fork, config, nth(1, EventKind::TrialIntent));
        let events = fork.events();
        intents(&events)[done].clone()
    };
    let strip = |intent: &TrialIntent| TrialIntent {
        trial: want.trial.clone(),
        kernel_cursor: None,
        kernel_error: None,
        ..intent.clone()
    };
    assert_eq!(strip(&next(got)), strip(&want));
    assert_ne!(strip(&next(Config::default())), strip(&want));
}

fn first_action(
    bench: &Bench,
    config: Config,
    known: usize,
) -> (EventKind, Option<Decision>, Option<CoreId>, Vec<Seq>) {
    run_until(
        bench,
        config,
        Box::new(|event| {
            if matches!(
                event.kind(),
                EventKind::TunerDecision | EventKind::TrialIntent
            ) {
                Observe::Stop
            } else {
                Observe::Continue
            }
        }),
    );
    let events = bench.events();
    let event = &events[known..]
        .iter()
        .find(|event| {
            matches!(
                event.kind(),
                EventKind::TunerDecision | EventKind::TrialIntent
            )
        })
        .unwrap();
    if let Payload::TunerDecision(decision) = &event.payload {
        (
            event.kind(),
            Some(decision.decision),
            Some(decision.core),
            event.cause.clone(),
        )
    } else if let Payload::TrialIntent(intent) = &event.payload {
        (event.kind(), None, intent.core, event.cause.clone())
    } else {
        unreachable!()
    }
}

#[test]
fn recorded_passes_count_only_under_the_recorded_backends() {
    let definition = hunt_machine(2);
    let recorded = recorded();
    let stopped = Bench::new(&definition, 1);
    run_until(&stopped, recorded.clone(), nth(2, EventKind::TrialEnd));
    let events = stopped.events();
    let known = events.len();
    let passes: Vec<Seq> = events
        .iter()
        .filter(|event| event.kind() == EventKind::TrialEnd)
        .map(|event| event.seq)
        .collect();
    let core = intents(&events)[0].core;

    let counted = first_action(&stopped.fork(&definition, 1), recorded, known);
    assert_eq!(
        counted,
        (
            EventKind::TunerDecision,
            Some(Decision::StepDeeper),
            core,
            passes
        )
    );

    let dropped = Config::parse(r#"{"durations": {"search_trial_s": 17}}"#).unwrap();
    let ignored = first_action(&stopped.fork(&definition, 1), dropped, known);
    assert_ne!(ignored.0, EventKind::TunerDecision);
}
