use std::num::NonZeroU32;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, Event, FixStamp, MemFs, Payload, SCHEMA, ShutdownReason};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Persistence, SimInput, events, simulate};

/// Generate a short valid journal for the fake simulator processes.
pub(crate) fn unfinished() -> Vec<Event> {
    let simulator = Simulator::new(
        Model::new(&MachineDefinition::default(), 1).unwrap(),
        RunSettings::default(),
    )
    .unwrap();
    let night_end = simulator.now() + SignedDuration::from_hours(8);
    let persistence = Persistence::Memory(MemFs::new());
    simulate(SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: Build {
            version: "0.0.0".into(),
            rev: None,
            ruleset: 1,
            schema: SCHEMA,
            fixes: FixStamp::none(),
            evidence: 1,
        },
        simulator,
        persistence: persistence.clone(),
        replay: togi_simrun::Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(1000).unwrap(),
        stop_at: Some(night_end),
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: Some(Box::new(|event| {
            if matches!(event.payload, Payload::TrialEnd(_)) {
                Observe::Stop
            } else {
                Observe::Continue
            }
        })),
        dump_to: None,
        adapt_trials: None,
    })
    .unwrap();
    let mut events = events(&persistence).unwrap();
    let terminal = events.pop().unwrap();
    let Payload::Shutdown(shutdown) = terminal.payload else {
        panic!("the short night must shut down");
    };
    assert_eq!(shutdown.reason, ShutdownReason::Signal);
    events
}
