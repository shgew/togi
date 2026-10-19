use std::num::NonZeroU32;
use std::path::Path;

use jiff::tz::TimeZone;
use proptest::prelude::*;
use togi_config::Config;
use togi_journal::{Build, Event, FixStamp, MemFs, StateDir};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, ReadState, SnapshotStatus, StopPolicy};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Persistence, SimInput, events, simulate};
use togi_tool_forecast::{Session, after, read_dir_fs};
use togi_watch::model::Model as Watch;

use super::{measure, measure_with};

fn generated(seed: u64, cores: i64) -> (Vec<Event>, MachineDefinition) {
    let definition = MachineDefinition {
        cores,
        ..MachineDefinition::default()
    };
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
        simulator: Simulator::new(
            Model::new(&definition, seed).unwrap(),
            RunSettings {
                seed,
                ..RunSettings::default()
            },
        ),
        persistence: persistence.clone(),
        replay: togi_simrun::Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(8).unwrap(),
        policy: StopPolicy {
            cycles: NonZeroU32::new(1),
            first_result: false,
        },
        write_samples: false,
        defects: &[],
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: None,
        dump_to: None,
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

fn forecast_equal(cached: &[Session], full: &[Session], anchor: &togi_tool_forecast::Anchor) {
    match (after(cached, anchor), after(full, anchor)) {
        (Ok(cached), Ok(full)) => assert_eq!(cached, full),
        (Err(cached), Err(full)) => assert_eq!(cached.to_string(), full.to_string()),
        (cached, full) => panic!("forecast cache changed outcome: {cached:?} versus {full:?}"),
    }
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
        seed in any::<u64>(), cores in prop::sample::select(vec![2_i64, 4]),
        cuts in prop::collection::vec(any::<usize>(), 1..5),
    ) {
        let (events, definition) = generated(seed, cores);
        let full = ReadState::replay(&events).unwrap();
        let model = Model::new(&definition, seed).unwrap();
        let measured = measure(&events, &model, None).unwrap();
        let expected_watch = togi_watch::model::project(&events);
        let forecast = Session::from_events(events.clone()).unwrap();
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", lines(&events));
        let directory = Path::new("/state");
        for raw in cuts.into_iter().chain([0, events.len()]) {
            let cut = raw % (events.len() + 1);
            publish(&fs, &events, cut);
            let restored = ReadState::read_in(&fs, directory, &events).unwrap();
            prop_assert_eq!(&restored, &full, "seed {}, cut {}", seed, cut);
            prop_assert_eq!(restored.tuner().forecast(), full.tuner().forecast(), "seed {}, cut {}", seed, cut);
            let watch = Watch::read_in(&fs, directory, &events);
            prop_assert_eq!(watch.snapshot(), expected_watch.snapshot(), "seed {}, cut {}", seed, cut);
            prop_assert_eq!(measure_with(&events, &model, None, Some(restored.facts())).unwrap(), measured.clone());
            let cached_forecast = read_dir_fs(&fs, directory, None).unwrap();
            for at in [0, cut.saturating_sub(1), events.len() - 1] {
                let anchor = forecast.anchor_at(events[at].seq).unwrap();
                forecast_equal(&cached_forecast, std::slice::from_ref(&forecast), &anchor);
            }
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
