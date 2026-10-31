#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "fixtures panic on failure; named kernel adapters exercise child signals and unsynced sample persistence"
)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use test_case::test_case;
use togi_config::Config;
use togi_journal::{Build, EventKind, FixStamp, Payload, Recording, SCHEMA, ShutdownReason};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::ConfigSource;
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
        .args(["--night-hours", "1000"])
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
    let simulator = Simulator::new(
        Model::new(&definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
    .unwrap();
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
        simulator: simulator.clone(),
        persistence: persistence.clone(),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(10).unwrap(),
        stop_at: Some(simulator.now() + SignedDuration::from_hours(8)),
        write_samples,
        defects: togi_defect::KNOWN_DEFECTS,
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
        adapt_trials: None,
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

#[test_case("boot_number", serde_json::json!(u32::MAX), "allocate the next simulated boot number"; "boot exhaustion")]
#[test_case("clock", serde_json::json!({"secs": u64::MAX, "nanos": 0}), "add elapsed time to the simulated epoch"; "elapsed overflow")]
#[test_case("wall_offset", serde_json::to_value(jiff::SignedDuration::from_secs(-1)).unwrap(), "apply the simulated wall-clock offset"; "lower timestamp overflow")]
fn real_process_simulator_arithmetic_errors_are_reported(
    field: &str,
    value: serde_json::Value,
    operation: &str,
) {
    // The boot exhaustion needs a crash to reboot at; four cores that crash past -10 give one in the first night.
    let definition: togi_sim::MachineDefinition = serde_json::from_str(
        r#"{
          "cores": 4,
          "model": {"past_limit_rate": 1.0, "signals": {"crash": 1}, "crash_mce": 0.0},
          "core": [
            {"id": 0, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]},
            {"id": 1, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]},
            {"id": 2, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]},
            {"id": 3, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]}
          ]
        }"#,
    )
    .unwrap();
    let sim = Simulator::new(
        Model::new(&definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let mut persisted = serde_json::to_value(sim.persisted()).unwrap();
    persisted[field] = value;
    if field == "wall_offset" {
        persisted["epoch"] = serde_json::to_value(jiff::Timestamp::MIN).unwrap();
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("machine.json"),
        serde_json::to_vec(&persisted).unwrap(),
    )
    .unwrap();
    let machine = tmp.path().join("hunt.json");
    std::fs::write(&machine, serde_json::to_vec(&definition).unwrap()).unwrap();
    let output = command()
        .arg("--state-dir")
        .arg(tmp.path())
        .arg("--machine")
        .arg(&machine)
        .args(["--max-boots", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(operation), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
