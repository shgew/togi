use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Read as _};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use togi_config::Config;
use togi_journal::{Build, Payload, SCHEMA, ShutdownReason};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Persistence, Replay, SimInput, simulate};

use crate::common::{command, entries, live_events, sim, sim_in_tmp};

#[test]
fn the_boot_cap_ends_an_unfinished_session_with_status_three() {
    let state = tempfile::tempdir().unwrap();
    let ran = sim([
        "--max-boots".as_ref(),
        "2".as_ref(),
        "--state-dir".as_ref(),
        state.path().as_os_str(),
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
fn without_a_state_directory_a_temporary_one_is_kept_and_announced_first() {
    let tmp = tempfile::tempdir().unwrap();
    let ran = sim_in_tmp(tmp.path(), ["--max-boots", "2"]);
    assert_eq!(ran.code, Some(3), "{}", ran.stderr);
    let created = entries(tmp.path());
    assert_eq!(created.len(), 1, "{created:?}");
    let dir = &created[0];
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
    let boots: BTreeSet<_> = live_events(dir)
        .into_iter()
        .map(|event| event.boot)
        .collect();
    assert_eq!(boots.len(), 2);
}

fn samples(dir: &Path, name: &str) -> usize {
    let trials = dir.join("trials");
    if !trials.exists() {
        return 0;
    }
    entries(&trials)
        .iter()
        .filter(|trial| trial.join(name).exists())
        .count()
}

#[test]
fn samples_reach_the_state_directory_only_when_asked_for() {
    for write in [false, true] {
        let state = tempfile::tempdir().unwrap();
        let mut args = vec!["--state-dir".as_ref(), state.path().as_os_str()];
        if write {
            args.push("--samples".as_ref());
        }
        let ran = sim(&args);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(samples(state.path(), "samples.jsonl.gz") > 0, write);
        assert_eq!(samples(state.path(), "samples.jsonl"), 0);
    }
}

#[test]
fn a_finished_session_ends_its_journal_with_the_cycles_shutdown() {
    let state = tempfile::tempdir().unwrap();
    let ran = sim(["--state-dir".as_ref(), state.path().as_os_str()]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let events = live_events(state.path());
    let Payload::Shutdown(shutdown) = &events.last().unwrap().payload else {
        panic!("the journal does not end in a shutdown");
    };
    assert_eq!(shutdown.reason, ShutdownReason::Cycles);
    assert!(state.path().join("state.json").exists());
    assert!(
        ran.stderr.lines().last().unwrap().contains("shutdown"),
        "{}",
        ran.stderr
    );
}

#[test]
fn a_signal_stops_the_session_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = command()
        .env("TMPDIR", tmp.path())
        .args(["--cycles", "1000"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut announced = String::new();
    stderr.read_line(&mut announced).unwrap();
    let dir = announced
        .strip_prefix("sim: state directory ")
        .unwrap()
        .trim_end()
        .to_owned();
    kill(
        Pid::from_raw(i32::try_from(child.id()).unwrap()),
        Signal::SIGTERM,
    )
    .unwrap();
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "{rest}");
    let events = live_events(Path::new(&dir));
    let Payload::Shutdown(shutdown) = &events.last().unwrap().payload else {
        panic!("the journal does not end in a shutdown");
    };
    assert_eq!(shutdown.reason, ShutdownReason::Signal);
}

fn build() -> Build {
    let rev = togi_version::rev();
    Build {
        version: togi_version::version().to_owned(),
        rev: (!rev.is_empty()).then(|| rev.to_owned()),
        ruleset: 1,
        schema: SCHEMA,
        fixes: 0,
        evidence: 1,
    }
}

fn record(dir: &Path, config: Config) {
    let machine = MachineDefinition::default();
    simulate(SimInput {
        config,
        source: ConfigSource::default(),
        build: build(),
        simulator: Simulator::new(
            Model::new(&machine, 1).unwrap(),
            RunSettings {
                seed: 1,
                ..RunSettings::default()
            },
        ),
        persistence: Persistence::Files(dir.to_path_buf()),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::MIN,
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
}

#[test]
fn a_resume_runs_under_the_configuration_the_journal_recorded() {
    let state = tempfile::tempdir().unwrap();
    let recorded = Config::parse(
        r#"{"durations":{"search_trial_s":60},"backends":{"mprime":"/nix/store/recorded-mprime","ycruncher":"/nix/store/recorded-ycruncher"}}"#,
    )
    .unwrap();
    assert_ne!(recorded.backends(), Config::default().backends());
    record(state.path(), recorded.clone());
    let ran = sim([
        "--state-dir".as_ref(),
        state.path().as_os_str(),
        "--max-boots".as_ref(),
        "1".as_ref(),
    ]);
    assert!(matches!(ran.code, Some(0 | 3)), "{}", ran.stderr);
    let loaded: Vec<Config> = live_events(state.path())
        .into_iter()
        .filter_map(|event| match event.payload {
            Payload::ConfigLoaded(loaded) => Some(loaded.config),
            _ => None,
        })
        .collect();
    assert!(loaded.len() >= 2, "{} config.loaded events", loaded.len());
    for config in &loaded {
        assert_eq!(
            config.backends().mprime(),
            Some(Path::new("/nix/store/recorded-mprime"))
        );
        assert_eq!(
            config.backends().ycruncher(),
            Some(Path::new("/nix/store/recorded-ycruncher"))
        );
        assert_eq!(*config, recorded);
    }
}

fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    for path in entries(dir) {
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push((path.clone(), std::fs::read(&path).unwrap()));
        }
    }
    found
}

#[test]
fn a_recorded_checking_cycle_that_is_not_full_refuses_cycles_before_touching_the_state() {
    let state = tempfile::tempdir().unwrap();
    let short = Config::parse(r#"{"checking":{"cycle":["R1","R2"]}}"#).unwrap();
    assert!(!short.checking_cycle().is_full());
    record(state.path(), short);
    let before = files(state.path());
    let ran = sim([
        "--state-dir".as_ref(),
        state.path().as_os_str(),
        "--max-boots".as_ref(),
        "5".as_ref(),
    ]);
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains("is not full"), "{}", ran.stderr);
    assert_eq!(files(state.path()), before);
}
