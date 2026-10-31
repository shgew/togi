//! Whole nights over the simulated machine and the in-memory journal, through the public `run`.

mod compare;
mod cut;

use jiff::{SignedDuration, Timestamp};
use togi_journal::{DeadEndCondition, Event, Payload, Queue, TrialIntent};
use togi_machine::Backend;
use togi_sim::MachineDefinition;

use crate::testing::Bench;
use crate::{Ended, RunInput, SessionError, Stop};

/// How long the operator lets a night run before ending it.
const NIGHT: SignedDuration = SignedDuration::from_hours(8);

fn bench() -> Bench {
    Bench::new(8)
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

fn count(events: &[Event], kind: &str) -> usize {
    events
        .iter()
        .filter(|event| event.kind().as_str() == kind)
        .count()
}

/// The run ended the way an operator ends a night, every core restored to CO 0 and the shutdown recorded.
fn ended_by_the_operator(bench: &Bench, ended: &Ended) {
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Signal));
    let last = ended.journal.events().last().unwrap();
    let Payload::Shutdown(shutdown) = &last.payload else {
        panic!("{:?}", last.payload);
    };
    assert_eq!(shutdown.reason, togi_journal::ShutdownReason::Signal);
    let mut smu = bench.sim.seams().smu;
    for n in 0..8 {
        assert_eq!(
            smu.offset(crate::testing::core(n)).unwrap().0,
            0,
            "core {n}"
        );
    }
}

/// A run whose operator ends it once the journal holds `events` events.
fn ended_at_seq(bench: &Bench, events: u64) -> RunInput<'static> {
    let mut input = bench.input();
    let cancel = input.cancel.clone();
    input.on_append = Some(Box::new(move |event| {
        if event.seq.get() >= events {
            cancel.cancel();
        }
    }));
    input
}

fn a_night(bench: &Bench) -> Timestamp {
    bench.sim.now() + NIGHT
}

fn backends_of(intent: &TrialIntent) -> Vec<Backend> {
    intent
        .content
        .iter()
        .map(|content| content.workload.backend())
        .collect()
}

#[test]
fn an_attested_night_maps_from_minus_five_without_an_all_zero_trial() {
    let bench = bench();
    let ended = crate::run(bench.input_ending_at(a_night(&bench)));

    ended_by_the_operator(&bench, &ended);
    let queues: Vec<Queue> = intents(ended.journal.events())
        .iter()
        .filter_map(|intent| intent.task.map(|task| task.queue))
        .collect();
    assert_eq!(queues.first(), Some(&Queue::Mapping), "{queues:?}");
    assert!(queues.iter().all(|queue| *queue != Queue::Control), "{queues:?}");
    let trials = intents(ended.journal.events());
    assert_eq!(trials.first().unwrap().offset.get(), -5);
    assert!(trials.iter().all(|trial| {
        trial.profile.iter().any(|(_, offset)| offset.get() != 0)
    }));
}

#[test]
fn unattended_run_without_operator_attestation_ends_named_and_at_stock() {
    let bench = bench();
    let ended = crate::run(bench.input_unattested());
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let Some(Stop::DeadEnd { dead_end, .. }) = &ended.stop else {
        panic!("{:?}", ended.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
    assert!(dead_end.detail.starts_with("stock_unattested:"));
    assert!(intents(ended.journal.events()).is_empty());
    assert_eq!(count(ended.journal.events(), "stock.attested"), 0);
    let mut smu = bench.sim.seams().smu;
    for core in 0..8 {
        assert_eq!(smu.offset(crate::testing::core(core)).unwrap().0, 0, "core {core}");
    }
}

#[test]
fn no_trial_starts_after_the_operator_ends_the_night() {
    let bench = bench();
    let end = bench.sim.now() + SignedDuration::from_mins(20);
    let ended = crate::run(bench.input_ending_at(end));

    ended_by_the_operator(&bench, &ended);
    let events = ended.journal.events();
    let ending = events
        .iter()
        .position(|event| event.time >= end)
        .expect("the operator ended the night");
    let late: Vec<_> = events[ending + 1..]
        .iter()
        .filter(|event| matches!(event.payload, Payload::TrialIntent(_)))
        .map(|event| event.seq)
        .collect();
    assert!(late.is_empty(), "trials after the end: {late:?}");
}

#[test]
fn a_restart_after_a_signal_opens_a_new_night() {
    let bench = bench();
    let interrupted = crate::run(ended_at_seq(&bench, 60));
    assert_eq!(interrupted.stop, Some(Stop::Signal));
    let before = intents(interrupted.journal.events()).len();
    assert!(before > 0);
    drop(interrupted.journal.close().unwrap());

    // The signal's shutdown closed the night: the restart opens the next one.
    let resumed = crate::run(bench.input_ending_at(a_night(&bench)));

    ended_by_the_operator(&bench, &resumed);
    let events = resumed.journal.events();
    let nights: Vec<_> = intents(events).iter().filter_map(|i| i.night).collect();
    assert!(nights.len() > before);
    let (first, second) = nights.split_at(before);
    assert!(
        first.windows(2).all(|pair| pair[0] == pair[1]),
        "{nights:?}"
    );
    assert!(
        second.windows(2).all(|pair| pair[0] == pair[1]),
        "{nights:?}"
    );
    assert_ne!(first[0], second[0]);
    assert_eq!(count(events, "session.start"), 1);
}

#[test]
fn a_night_continues_across_a_crash_reboot() {
    let bench = bench();
    let end = a_night(&bench);
    bench.sim.crash_at_write(12);
    let crashed = crate::run(bench.input_ending_at(end));
    assert!(
        crashed.errors.iter().any(SessionError::is_crash),
        "{:?} {:?}",
        crashed.stop,
        crashed.errors
    );
    let before = intents(crashed.journal.events()).len();
    drop(crashed.journal.close().unwrap());
    bench.sim.reboot().unwrap();

    let resumed = crate::run(bench.input_ending_at(end));

    ended_by_the_operator(&bench, &resumed);
    let events = resumed.journal.events();
    assert_eq!(count(events, "crash.detected"), 1);
    assert!(intents(events).len() > before);
    let nights: Vec<_> = intents(events).iter().map(|intent| intent.night).collect();
    assert!(
        nights.windows(2).all(|pair| pair[0] == pair[1]),
        "{nights:?}"
    );
}

#[test]
fn a_restart_reads_back_the_state_a_mapping_night_published_without_rebuilding_it() {
    let bench = bench();
    let first = crate::run(bench.input_ending_at(a_night(&bench)));
    ended_by_the_operator(&bench, &first);
    assert!(
        intents(first.journal.events())
            .iter()
            .filter(|intent| intent.task.is_some_and(|task| task.queue == Queue::Mapping))
            .count()
            > 2
    );
    drop(first.journal.close().unwrap());

    let resumed = crate::run(bench.input_ending_at(bench.short_night()));

    let rebuilt: Vec<_> = resumed
        .journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::StateRebuilt(rebuilt) = &event.payload {
                Some(rebuilt.fields.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(rebuilt.is_empty(), "{rebuilt:?}");
}

#[test]
fn a_backend_inconclusive_through_every_retry_is_left_out_and_the_night_goes_on() {
    let bench = bench();
    // Controls need every backend before any nonzero work, so the backend fails once a night has passed them.
    let mut input = bench.input();
    let cancel = input.cancel.clone();
    input.on_append = Some(Box::new(move |event| {
        if let Payload::TrialIntent(intent) = &event.payload
            && intent.task.is_some_and(|task| task.queue == Queue::Mapping)
        {
            cancel.cancel();
        }
    }));
    let controlled = crate::run(input);
    assert_eq!(controlled.stop, Some(Stop::Signal));
    drop(controlled.journal.close().unwrap());
    bench.sim.fail_backend_setup(Backend::Mprime);

    let ended = crate::run(bench.input_ending_at(a_night(&bench)));

    ended_by_the_operator(&bench, &ended);
    let events = ended.journal.events();
    let retries = count(events, "backend.retry");
    assert_eq!(retries, 3, "one retry after each of 1, 5 and 30 min");
    let last_retry = events
        .iter()
        .rposition(|event| event.kind().as_str() == "backend.retry")
        .unwrap();
    let after: Vec<Vec<Backend>> = intents(&events[last_retry..])
        .iter()
        .map(|intent| backends_of(intent))
        .collect();
    let mprime = after
        .iter()
        .filter(|backends| backends.contains(&Backend::Mprime))
        .count();
    assert!(mprime <= 1, "the exhausting attempt at most: {after:?}");
    assert!(
        after
            .iter()
            .filter(|backends| !backends.contains(&Backend::Mprime))
            .count()
            > 1,
        "the night goes on without mprime: {after:?}"
    );
    assert_eq!(count(events, "deadend"), 0);
}

#[test]
fn every_backend_inconclusive_through_every_retry_ends_the_night_without_evidence() {
    let bench = bench();
    bench.sim.fail_backend_setup(Backend::Mprime);
    bench.sim.fail_backend_setup(Backend::Ycruncher);
    let ended = crate::run(bench.input());

    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let Some(Stop::DeadEnd {
        dead_end, causes, ..
    }) = &ended.stop
    else {
        panic!("{:?}", ended.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
    assert!(!causes.is_empty());
    let events = ended.journal.events();
    assert_eq!(count(events, "deadend"), 1);
}

#[test]
fn a_baseline_that_is_not_all_zero_is_refused_before_anything_is_written() {
    let bench = Bench::defined(&MachineDefinition {
        cores: 2,
        bios: Some(vec![-5, 0]),
        ..MachineDefinition::default()
    });
    let ended = crate::run(bench.input());

    assert!(
        matches!(
            ended.errors.first(),
            Some(SessionError::BaselineNotZero { .. })
        ),
        "{:?}",
        ended.errors
    );
    let events = ended.journal.events();
    assert_eq!(count(events, "session.baseline"), 0);
    assert_eq!(count(events, "trial.intent"), 0);
    assert_eq!(count(events, "profile.applied"), 0);
}

#[test]
fn a_bios_change_between_boots_records_a_new_context_and_the_session_continues() {
    let bench = bench();
    let first = crate::run(ended_at_seq(&bench, 60));
    assert_eq!(first.stop, Some(Stop::Signal));
    drop(first.journal.close().unwrap());
    bench.sim.reboot().unwrap();
    let mut context = bench.sim.seams().host.bios_context().unwrap();
    context.bios_version.push_str("-next");
    bench.sim.set_bios_context(context.clone());

    let resumed = crate::run(bench.input_ending_at(a_night(&bench)));

    ended_by_the_operator(&bench, &resumed);
    let events = resumed.journal.events();
    let recorded: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::SessionContext(recorded) = &event.payload {
                Some(recorded.bios_context())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[1], context);
    assert_eq!(count(events, "session.start"), 1);
}
