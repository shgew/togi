use std::num::NonZeroU32;
use std::path::Path;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use proptest::prelude::*;
use togi_config::Config;
use togi_journal::{Build, Event, FixStamp, MemFs, StateDir};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, ReadState, SnapshotStatus};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Observer, Persistence, SimInput, events, simulate};
use togi_watch::model::Model as Watch;

use super::{measure, measure_with};

/// Four cores that crash past -10: a Ruleset 13 night needs a neighbour on every CCD, and this one crashes.
fn hunting() -> MachineDefinition {
    let core = |id: i64| {
        serde_json::json!({
            "id": id,
            "alone": [-10, -10, -10, -10, -10],
            "together": [-10, -10, -10, -10, -10, -10, -50],
        })
    };
    serde_json::from_value(serde_json::json!({
        "cores": 4,
        "model": {"past_limit_rate": 1.0, "signals": {"crash": 1}, "crash_mce": 0.0},
        "core": [core(0), core(1), core(2), core(3)],
    }))
    .unwrap()
}

fn generated(
    seed: u64,
    cores: i64,
    minutes: i64,
    crashing: bool,
) -> (Vec<Event>, MachineDefinition) {
    let definition = if crashing {
        hunting()
    } else {
        MachineDefinition {
            cores,
            ..MachineDefinition::default()
        }
    };
    generated_from(definition, seed, SignedDuration::from_mins(minutes), None)
}

fn generated_from(
    definition: MachineDefinition,
    seed: u64,
    duration: SignedDuration,
    observer: Option<Observer>,
) -> (Vec<Event>, MachineDefinition) {
    let simulator = Simulator::new(
        Model::new(&definition, seed).unwrap(),
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    )
    .unwrap();
    let night_end = simulator.now() + duration;
    let persistence = Persistence::Memory(MemFs::new());
    simulate(SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: Build {
            version: "0.0.0".into(),
            rev: None,
            ruleset: togi_tuner::RULESET,
            schema: togi_journal::SCHEMA,
            fixes: FixStamp::none(),
            evidence: togi_tuner::EVIDENCE_EPOCH,
        },
        simulator,
        persistence: persistence.clone(),
        replay: togi_simrun::Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(8).unwrap(),
        stop_at: Some(night_end),
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer,
        dump_to: None,
        adapt_trials: None,
    })
    .unwrap();
    (events(&persistence).unwrap(), definition)
}

fn lines(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .flat_map(|event| {
            let mut line = event.encode().unwrap().into_bytes();
            line.push(b'\n');
            line
        })
        .collect()
}

fn publish(fs: &MemFs, events: &[Event], cut: usize) {
    let state = ReadState::replay(&events[..cut]).unwrap();
    let prefix = togi_journal::snapshot::Prefix::of(
        &lines(&events[..cut]),
        events.get(cut.wrapping_sub(1)).map(|event| event.seq),
    );
    let document = togi_journal::snapshot::seal(prefix, &state.document()).unwrap();
    fs.put("/state/state.json", serde_json::to_vec(&document).unwrap());
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 12,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(621),
        ..ProptestConfig::default()
    })]
    #[test]
    fn snapshot_plus_random_tail_equals_full_replay_for_every_reader(
        seed in any::<u64>(), cores in prop::sample::select(vec![4_i64, 6]),
        minutes in 15_i64..91, crashing in any::<bool>(),
        cuts in prop::collection::vec(any::<usize>(), 1..5),
    ) {
        let (events, definition) = generated(seed, cores, minutes, crashing);
        let full = ReadState::replay(&events).unwrap();
        let model = Model::new(&definition, seed).unwrap();
        let measured = measure(&events, &model, None).unwrap();
        let expected_watch = togi_watch::model::project(&events);
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", lines(&events));
        let directory = Path::new("/state");
        for raw in cuts.into_iter().chain([0, events.len()]) {
            let cut = raw % (events.len() + 1);
            publish(&fs, &events, cut);
            let restored = ReadState::read_in(&fs, directory, &events).unwrap();
            prop_assert_eq!(&restored, &full, "seed {}, cut {}", seed, cut);
            let watch = Watch::read_in(&fs, directory, &events);
            prop_assert_eq!(watch.snapshot(), expected_watch.snapshot(), "seed {}, cut {}", seed, cut);
            prop_assert_eq!(measure_with(&events, &model, None, Some(restored.facts())).unwrap(), measured.clone());
            #[expect(clippy::disallowed_methods, reason = "the snapshot property uses only MemFs")]
            let dir = StateDir::simulated(fs.clone()).unwrap();
            let status = togi_session::verify_snapshot(&dir, &events).unwrap();
            prop_assert_eq!(status, if cut == events.len() { SnapshotStatus::Matches } else { SnapshotStatus::Stale(events.get(cut.wrapping_sub(1)).map(|event| event.seq)) });
            let mutations = fs.mutations();
            let _ = ReadState::in_dir(&dir, &events).unwrap();
            let _ = togi_tool_audit::audit_recordings(&dir, togi_tool_audit::Completion::InProgress).unwrap();
            prop_assert_eq!(fs.mutations(), mutations, "readers must not rewrite a cache");
        }
    }
}

#[test_case::test_case(false; "shutdown")]
#[test_case::test_case(true; "detected crash")]
fn snapshot_then_config_reload_has_the_same_cleared_trial_end_as_full_replay(crash: bool) {
    // Detect the crash through the durable journal hook rather than finishing the night.
    let observer: Option<Observer> = crash.then(|| {
        Box::new(|event: &Event| {
            if matches!(event.payload, togi_journal::Payload::CrashDetected(_)) {
                Observe::Stop
            } else {
                Observe::Continue
            }
        }) as Observer
    });
    let (events, _) = generated_from(hunting(), 1, SignedDuration::from_hours(8), observer);
    let cut = events
        .iter()
        .rposition(|event| {
            if crash {
                matches!(event.payload, togi_journal::Payload::CrashDetected(_))
            } else {
                matches!(event.payload, togi_journal::Payload::Shutdown(_))
            }
        })
        .expect("the simulation must reach the requested snapshot boundary")
        + 1;
    let config = events
        .iter()
        .find_map(|event| {
            matches!(event.payload, togi_journal::Payload::ConfigLoaded(_))
                .then(|| event.payload.clone())
        })
        .unwrap();
    // A stopped simulation still appends cancellation cleanup after the crash boundary.
    let mut events = events[..cut].to_vec();
    let last = events.last().unwrap();
    if crash {
        assert!(matches!(
            last.payload,
            togi_journal::Payload::CrashDetected(_)
        ));
    }
    let reload = Event::new(
        last.seq.next().unwrap(),
        togi_journal::Stamp {
            time: last.time,
            mono: None,
        },
        last.boot.clone(),
        Vec::new(),
        config,
    );
    events.push(reload);
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", lines(&events));
    publish(&fs, &events, cut);
    let full = togi_watch::model::project(&events);
    let restored = Watch::read_in(&fs, Path::new("/state"), &events);
    assert!(full.snapshot().ended.is_none());
    assert_eq!(restored.snapshot(), full.snapshot());
}
