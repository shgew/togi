use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::Path;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use test_case::test_case;
use togi_config::Config;
use togi_journal::{DeadEndCondition, MemFs, Payload, ShutdownReason};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Persistence, Replay, SimInput, simulate};

use super::common::Host;

#[test]
fn the_boot_cap_ends_an_unfinished_session_with_status_three() {
    let ran = Host::hunting().run([
        "--machine",
        "/hunt.json",
        "--max-boots",
        "2",
        "--state-dir",
        "/state",
    ]);
    assert_eq!(ran.code, Some(3), "{}", ran.stderr);
    assert!(
        ran.stderr.ends_with(
            "sim: the simulated machine reached its boot cap without stopping after 2 boots\n"
        ),
        "{}",
        ran.stderr
    );
}

#[test]
fn a_simulation_failure_prints_its_operation_path_and_cause_once() {
    let ran = Host::default().execute(["--state-dir", "/state"], |input| {
        let Persistence::Memory(fs) = &input.persistence else {
            unreachable!()
        };
        fs.inject(togi_journal::fs::Fault::before(togi_journal::fs::Op::Open, 1).named("lock"));
        simulate(input)
    });
    assert_eq!(ran.code, Some(1));
    assert_eq!(ran.stdout, "");
    assert_eq!(
        ran.stderr,
        "sim: lock the state directory: lock /state/lock: injected Open fault\n"
    );
}

#[test]
fn without_a_state_directory_a_temporary_one_is_kept_and_announced_first() {
    let mut host = Host::hunting();
    let ran = host.run(["--machine", "/hunt.json", "--max-boots", "2"]);
    assert_eq!(ran.code, Some(3), "{}", ran.stderr);
    assert_eq!(host.states.len(), 1);
    let dir = host.states.keys().next().unwrap();
    assert!(
        dir.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("togi-sim-")
    );
    assert_eq!(
        ran.stderr.lines().next().unwrap(),
        format!("sim: state directory {}", dir.display())
    );
    let boots: BTreeSet<_> = host
        .events(dir.to_str().unwrap())
        .into_iter()
        .map(|event| event.boot)
        .collect();
    assert_eq!(boots.len(), 2);
}

#[test_case(false; "without the flag")]
#[test_case(true; "with the flag")]
fn samples_reach_the_state_directory_only_when_asked_for(write: bool) {
    let mut host = Host::default();
    let mut args = vec!["--state-dir", "/state"];
    if write {
        args.push("--samples");
    }
    let mut called = false;
    let ran = host.execute(args, |mut input| {
        called = true;
        assert_eq!(input.write_samples, write);
        assert!(matches!(&input.persistence, Persistence::Memory(_)));
        // Record the disk-output request at the execution seam, then use the simulator's existing
        // in-memory sample collection for the whole session. The disk adapter is exercised separately.
        input.write_samples = false;
        let simulator = input.simulator.clone();
        let persistence = input.persistence.clone();
        let outcome = simulate(input)?;
        let trial = togi_simrun::events(&persistence)
            .unwrap()
            .into_iter()
            .rev()
            .find_map(|event| {
                if let Payload::TrialEnd(end) = event.payload {
                    Some(end.trial)
                } else {
                    None
                }
            })
            .expect("the completed session ended a trial");
        assert!(
            simulator
                .seams()
                .trials
                .samples(trial.as_str())
                .next()
                .is_some()
        );
        Ok(outcome)
    });
    assert!(called);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
}

#[test]
fn a_finished_session_ends_its_journal_with_the_operators_shutdown() {
    let mut host = Host::default();
    let ran = host.run(["--state-dir", "/state"]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let events = host.events("/state");
    let Payload::Shutdown(shutdown) = &events.last().unwrap().payload else {
        panic!("the journal does not end in a shutdown");
    };
    assert_eq!(shutdown.reason, ShutdownReason::Signal);
    assert!(
        host.states[Path::new("/state")]
            .contents("/state/state.json")
            .is_some()
    );
    assert!(
        ran.stderr.lines().last().unwrap().contains("shutdown"),
        "{}",
        ran.stderr
    );
}

#[test]
fn a_rerun_after_a_closed_night_runs_a_fresh_night() {
    let mut host = Host::default();
    let args = ["--state-dir", "/state", "--night-hours", "1"];
    assert_eq!(host.run(args).code, Some(0));
    let first = host.events("/state").len();
    let ran = host.run(args);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let events = host.events("/state");
    let second = &events[first..];
    let elapsed = second.last().unwrap().time.duration_since(second[0].time);
    assert!(
        elapsed >= SignedDuration::from_hours(1),
        "the second night ran only {elapsed:?}"
    );
}

#[test]
fn a_restart_after_a_decided_stop_starts_a_fresh_night() {
    let mut host = Host::default();
    host.put("/machine.json", THERMAL_MACHINE);
    let args = ["--machine", "/machine.json", "--state-dir", "/state"];
    assert_eq!(host.run(args).code, Some(1));
    let first = host.events("/state").len();
    assert_eq!(host.run(args).code, Some(1));
    let events = host.events("/state");
    let Payload::Shutdown(stop) = &events[first - 1].payload else {
        panic!("the first run does not end in a shutdown");
    };
    assert_eq!(stop.reason, ShutdownReason::DeadEnd);
    let now = events.last().unwrap().time + SignedDuration::from_hours(1);
    // The dead end's shutdown closed the first night: a run after it opens a fresh one.
    assert_eq!(
        crate::session::night_started(&events[..first], now).unwrap(),
        now
    );
    // Until its own stop, the restart's night starts at the restart, not at the closed night's start.
    let (last, unfinished) = events.split_last().unwrap();
    assert!(matches!(last.payload, Payload::Shutdown(_)));
    assert_eq!(
        crate::session::night_started(unfinished, now).unwrap(),
        events[first].time
    );
}

fn record(fs: MemFs, config: Config) {
    let simulator = Simulator::new(
        Model::new(&MachineDefinition::default(), 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
    .unwrap();
    simulate(SimInput {
        config,
        source: ConfigSource::default(),
        build: crate::build(),
        simulator: simulator.clone(),
        persistence: Persistence::Memory(fs),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(1000).unwrap(),
        stop_at: Some(simulator.now() + SignedDuration::from_hours(1)),
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: None,
        dump_to: None,
        adapt_trials: None,
    })
    .unwrap();
}

#[test]
fn a_resume_runs_under_the_configuration_the_journal_recorded() {
    let mut host = Host::default();
    let recorded = Config::parse(
        r#"{"backends":{"mprime":"/recorded-mprime","ycruncher":"/recorded-ycruncher"}}"#,
    )
    .unwrap();
    assert_ne!(recorded.backends(), Config::default().backends());
    let fs = MemFs::new();
    record(fs.clone(), recorded.clone());
    host.states.insert("/state".into(), fs);
    let ran = host.run(["--state-dir", "/state", "--max-boots", "1"]);
    assert!(matches!(ran.code, Some(0 | 3)), "{}", ran.stderr);
    let loaded: Vec<Config> = host
        .events("/state")
        .into_iter()
        .filter_map(|event| {
            if let Payload::ConfigLoaded(loaded) = event.payload {
                Some(Config::from_snapshot(&loaded.config).unwrap())
            } else {
                None
            }
        })
        .collect();
    assert!(loaded.len() >= 2, "{} config.loaded events", loaded.len());
    for config in loaded {
        assert_eq!(config, recorded);
    }
}

const THERMAL_MACHINE: &str = r#"{
  "cores": 4,
  "model": {"signals": {"crash": 1}, "reset": {"thermal_trip": 1}},
  "core": [
    {"id": 0, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -10]},
    {"id": 1, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -10]},
    {"id": 2, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -10]},
    {"id": 3, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -10]}
  ]
}"#;

#[test]
fn a_dead_end_exits_one_and_cites_the_events_that_caused_it() {
    let mut host = Host::default();
    host.put("/machine.json", THERMAL_MACHINE);
    let ran = host.run(["--machine", "/machine.json", "--state-dir", "/state"]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    let events = host.events("/state");
    let (dead_end, causes) = events
        .iter()
        .find_map(|event| {
            if let Payload::DeadEnd(dead_end) = &event.payload {
                Some((dead_end, &event.cause))
            } else {
                None
            }
        })
        .expect("the journal has no dead end");
    assert_eq!(dead_end.condition, DeadEndCondition::ThermalTrip);
    assert!(!causes.is_empty());
    let report: Vec<&str> = ran
        .stderr
        .lines()
        .skip_while(|line| !line.starts_with("sim: dead end "))
        .collect();
    assert_eq!(
        report[0],
        format!("sim: dead end {}: {}", dead_end.condition, dead_end.detail)
    );
    assert_eq!(report[1..].len(), causes.len(), "{}", ran.stderr);
    for (line, seq) in report[1..].iter().zip(causes) {
        let event = events.iter().find(|event| event.seq == *seq).unwrap();
        let rest = line.strip_prefix("  evidence: ").expect(line);
        assert!(
            rest.ends_with(&event.msg),
            "{rest} does not show seq {seq:?}: {}",
            event.msg
        );
    }
}
