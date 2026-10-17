#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "fixtures panic on failure; named kernel adapters exercise child signals and unsynced sample persistence"
)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;

use jiff::tz::TimeZone;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use test_case::test_case;
use togi_config::Config;
use togi_journal::{Build, EventKind, FixStamp, Payload, Recording, SCHEMA, ShutdownReason};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::{Model, RunSettings, Simulator};
use togi_simrun::{Observe, Persistence, Replay, SimInput, SimOutcome};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-sim"));
    command.env_remove("NO_COLOR").env_remove("JOURNAL_STREAM");
    command
}

#[test]
fn real_process_a_signal_stops_the_session_in_order() {
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
    assert_eq!(child.wait().unwrap().code(), Some(0), "{rest}");
    let recording = Recording::read(&Path::new(&dir).join("events.jsonl")).unwrap();
    assert!(recording.torn().is_none());
    let Payload::Shutdown(shutdown) = &recording.events().last().unwrap().payload else {
        panic!("the journal does not end in a shutdown");
    };
    assert_eq!(shutdown.reason, ShutdownReason::Signal);
}

#[cfg(target_os = "linux")]
#[test]
fn real_process_a_log_that_cannot_be_flushed_fails_a_command_that_otherwise_succeeds() {
    assert_eq!(
        command().arg("--help").output().unwrap().status.code(),
        Some(0)
    );
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    assert_eq!(
        command()
            .arg("--help")
            .stderr(full)
            .status()
            .unwrap()
            .code(),
        Some(1)
    );
}

/// One completed trial exercises journal publication and the simulator's unsynced compression adapter.
/// Whole-session behavior and sample-option forwarding are covered in memory, not repeated on disk.
#[test_case(false; "without samples")]
#[test_case(true; "with samples")]
fn real_fs_a_persisted_trial_publishes_state_and_compressed_samples(write_samples: bool) {
    let definition = togi_sim::MachineDefinition::default();
    let dir = tempfile::tempdir().unwrap();
    let persistence = Persistence::Files(dir.path().to_owned());
    let outcome = togi_simrun::simulate(SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: Build {
            version: togi_version::version().to_owned(),
            rev: None,
            ruleset: togi_tuner::RULESET,
            schema: SCHEMA,
            fixes: FixStamp::none(),
            evidence: togi_tuner::EVIDENCE_EPOCH,
        },
        simulator: Simulator::new(
            Model::new(&definition, 1).unwrap(),
            RunSettings {
                seed: 1,
                ..RunSettings::default()
            },
        ),
        persistence: persistence.clone(),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(10).unwrap(),
        policy: StopPolicy {
            cycles: NonZeroU32::new(1),
            first_result: false,
        },
        write_samples,
        defects: &[],
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: Some(Box::new(|event| {
            if event.kind() == EventKind::TrialEnd {
                Observe::Stop
            } else {
                Observe::Continue
            }
        })),
        dump_to: None,
    })
    .unwrap();
    assert!(matches!(outcome, SimOutcome::Stopped(_)));
    assert!(dir.path().join("state.json").is_file());
    let events = togi_simrun::events(&persistence).unwrap();
    let trial = events
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end.trial.as_str())
            } else {
                None
            }
        })
        .unwrap();
    let trial = dir.path().join("trials").join(trial);
    let compressed = trial.join("samples.jsonl.gz");
    assert_eq!(compressed.is_file(), write_samples);
    assert!(!trial.join("samples.jsonl").exists());
    if write_samples {
        assert!(
            std::fs::read(compressed)
                .unwrap()
                .starts_with(&[0x1f, 0x8b])
        );
    }
}
