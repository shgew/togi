use togi_config::Config;
use togi_journal::{Event, EventKind, Payload, TrialIntent};
use togi_simrun::{Observe, SimOutcome, simulate};

use crate::common::{Bench, crash_at_nth, finish, hunt_machine, nz};

fn recorded() -> Config {
    Config::parse(
        r#"{
            "dead_ends": {"inconclusive_in_a_row": 7},
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
    let fresh = Config::parse(r#"{"dead_ends": {"inconclusive_in_a_row": 5}}"#).unwrap();
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

    // A crash, not an operator stop: a stop ends the night, and the restart would open another one.
    let stopped = Bench::new(&definition, 1);
    let mut input = stopped.input();
    input.config = recorded.clone();
    input.max_boots = nz(1);
    input.observer = Some(crash_at_nth(&stopped.sim, EventKind::TrialEnd, 2));
    assert!(matches!(
        simulate(input).unwrap(),
        SimOutcome::Capped { .. }
    ));
    let stopped_events = stopped.events();
    let done = intents(&stopped_events).len();
    assert!(done >= 2);

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
}
