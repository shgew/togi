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
use togi_machine::{CancellationToken, TargetOffset};
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::{EPOCH, History, ScriptedMachine, ScriptedTopology, Simulator, TrialOutcome};
use togi_simrun::{Observe, Persistence, Replay, SimInput, SimOutcome};
use togi_tool_sim::scripted::{CONTRACT_VERSION, ScriptedCase, developer_machine};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-sim"));
    command.env_remove("NO_COLOR").env_remove("JOURNAL_STREAM");
    command
}

#[test]
fn real_process_the_scripted_contract_prints_only_its_version() {
    let output = command().arg("scripted-contract").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert_eq!(output.stdout, b"1\n");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

#[test]
fn real_process_a_wrong_scripted_contract_leaves_state_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let output = command()
        .arg("scripted")
        .args(["--case", "mapping-stop", "--contract", "01"])
        .arg("--state-dir")
        .arg(&state)
        .args(["--max-boots", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("contract"), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(!state.exists());
}

// A successful child also checks the selected case's required journal milestones. This exercises
// the actual disk-backed command rather than the substitute children used by the bench tests.
#[test_case(ScriptedCase::MappingStop, ShutdownReason::Signal; "mapping and stop")]
#[test_case(ScriptedCase::MceResume, ShutdownReason::Signal; "uncorrected mce and resume")]
#[test_case(ScriptedCase::PowerButtonReset, ShutdownReason::Signal; "power button and resume")]
#[test_case(ScriptedCase::ProfilePlacement, ShutdownReason::Signal; "day profile and placement")]
#[test_case(ScriptedCase::NoNonzeroWork, ShutdownReason::DeadEnd; "dead end")]
fn real_process_each_script_leaves_a_complete_journal(case: ScriptedCase, reason: ShutdownReason) {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let output = command()
        .arg("scripted")
        .args(["--case", case.name(), "--contract", CONTRACT_VERSION])
        .arg("--state-dir")
        .arg(&state)
        .arg("--max-boots")
        .arg(case.max_boots().to_string())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    let recording = Recording::read(&state.join("events.jsonl")).unwrap();
    assert!(recording.torn().is_none());
    assert!(matches!(&recording.events().last().unwrap().payload,
        Payload::Shutdown(end) if end.reason == reason));
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
    let script = ScriptedMachine::new(
        ScriptedTopology::SixteenCores,
        TargetOffset::ZERO,
        |_, _| TrialOutcome::Pass,
    );
    let simulator = Simulator::scripted(script, 1, History::default()).unwrap();
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

#[test_case("clock", serde_json::json!({"secs": u64::MAX, "nanos": 0}), "add elapsed time to the simulated epoch", EPOCH.checked_add(std::time::Duration::from_secs(u64::MAX)).unwrap_err(); "elapsed overflow")]
#[test_case("wall_offset", serde_json::to_value(SignedDuration::from_secs(-1)).unwrap(), "apply the simulated wall-clock offset", jiff::Timestamp::MIN.checked_add(SignedDuration::from_secs(-1)).unwrap_err(); "lower timestamp overflow")]
fn real_process_simulator_arithmetic_errors_are_reported(
    field: &str,
    value: serde_json::Value,
    operation: &str,
    cause: jiff::Error,
) {
    let sim = Simulator::scripted(developer_machine(), 1, History::default()).unwrap();
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
    let output = command()
        .arg("--state-dir")
        .arg(tmp.path())
        .args(["--max-boots", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    let cause = togi_machine::error_chain(&cause);
    assert_eq!(
        stderr,
        format!(
            "sim: resume the simulated machine: restore the simulated machine: restore the simulated clock: {operation}: {cause}\n"
        )
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}
