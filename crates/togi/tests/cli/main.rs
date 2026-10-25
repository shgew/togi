#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "fixtures panic on failure; the real_process_ shell tests execute real commands"
)]

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};

use test_case::test_case;
use togi_journal::fs::Fs;

#[path = "../support/mod.rs"]
mod support;

macro_rules! snapshot {
    ($name:ident, $actual:expr) => {{
        let expected = include_str!(concat!("snapshots/cli__", stringify!($name), ".snap"))
            .split_once("\n---\n")
            .unwrap()
            .1;
        assert_eq!(
            $actual.trim_end_matches('\n'),
            expected.trim_end_matches('\n')
        );
    }};
}

fn togi(args: &[&str]) -> Output {
    togi_on(&support::Host::new(), args)
}

fn togi_on(host: &support::Host, args: &[&str]) -> Output {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = togi_cli::run_with(
        std::iter::once("togi").chain(args.iter().copied()),
        &mut stdout,
        &mut stderr,
        host,
    );
    Output {
        status: ExitStatus::from_raw(i32::from(exit.code()) << 8),
        stdout,
        stderr,
    }
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).unwrap()
}

fn is_version_line(line: &str) -> bool {
    let Some(rest) = line
        .strip_prefix("togi ")
        .and_then(|r| r.strip_suffix('\n'))
    else {
        return false;
    };
    let Some((version, revision)) = rest.split_once('+') else {
        return false;
    };
    let numbers: Vec<&str> = version.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && !revision.is_empty()
        && revision
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[test]
fn version_prints_the_version_and_revision_on_stdout() {
    let ran = togi(&["--version"]);
    assert_eq!(ran.status.code(), Some(0));
    assert!(is_version_line(stdout(&ran)), "{:?}", stdout(&ran));
    assert_eq!(stderr(&ran), "");
}

#[test]
fn a_local_build_reports_the_dev_revision() {
    if option_env!("TOGI_REV").is_some_and(|rev| !rev.is_empty()) {
        return;
    }
    assert!(stdout(&togi(&["--version"])).ends_with("+dev\n"));
}

#[test]
fn version_after_a_command_is_a_usage_error() {
    let ran = togi(&["completion", "bash", "--version"]);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(stdout(&ran), "");
    assert!(
        stderr(&ran).contains("unexpected argument '--version'"),
        "{}",
        stderr(&ran)
    );
}

#[test]
fn top_level_help() {
    let ran = togi(&["--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(stderr(&ran), "");
    snapshot!(top_level_help, stdout(&ran));
}

#[test]
fn completion_help() {
    let ran = togi(&["completion", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(stderr(&ran), "");
    snapshot!(completion_help, stdout(&ran));
}

#[test]
fn no_command_prints_the_wordmark_and_usage_on_stderr() {
    let ran = togi(&[]);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(stdout(&ran), "");
    snapshot!(
        no_command_prints_the_wordmark_and_usage_on_stderr,
        stderr(&ran)
    );
}

#[test_case(&["--state-dir", "/x"] ; "separate value")]
#[test_case(&["--state-dir=/x"] ; "attached value")]
fn state_dir_without_a_command_prints_the_wordmark_and_usage_on_stderr(args: &[&str]) {
    let ran = togi(args);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(stdout(&ran), "");
    assert!(
        stderr(&ran).starts_with("   __              _\n"),
        "{}",
        stderr(&ran)
    );
    assert!(stderr(&ran).contains("Usage: togi"));
}

/// `/dev/full` fails every write with `ENOSPC`. A stdout closed at startup is not tested: Rust's
/// runtime reopens it on `/dev/null` before `main`, so no write ever fails there.
#[cfg(target_os = "linux")]
fn full(args: &[&str]) -> Output {
    let stdout = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    Command::new(env!("CARGO_BIN_EXE_togi"))
        .args(args)
        .stdout(stdout)
        .output()
        .unwrap()
}

#[test_case(&["completion", "bash"] ; "bash script")]
#[test_case(&["completion", "nushell"] ; "nushell script")]
#[test_case(&["--help"] ; "help")]
#[test_case(&["completion", "--help"] ; "command help")]
#[test_case(&["--version"] ; "version")]
#[cfg(target_os = "linux")]
fn real_process_a_stdout_that_cannot_be_written_exits_1_with_the_error_on_stderr(args: &[&str]) {
    let ran = full(args);
    assert_eq!(ran.status.code(), Some(1), "{}", stderr(&ran));
    assert!(
        stderr(&ran).starts_with("togi: ") && stderr(&ran).contains("No space left on device"),
        "{}",
        stderr(&ran)
    );
}

#[test]
#[cfg(target_os = "linux")]
fn real_process_a_usage_error_on_a_full_stdout_stays_exit_2() {
    let ran = full(&["bogus"]);
    assert_eq!(ran.status.code(), Some(2), "a usage error stays exit 2");
    assert!(stderr(&ran).contains("unrecognized subcommand"));
}

const SCOPE: &str = "--config applies only to doctor, run and reset";

#[test_case(&["bogus"], "unrecognized subcommand 'bogus'" ; "unknown subcommand")]
#[test_case(&["--bogus"], "unexpected argument '--bogus'" ; "unknown flag")]
#[test_case(&["--state-dir"], "a value is required for '--state-dir <path>'" ; "state dir without a value")]
#[test_case(&["completion"], "required arguments were not provided" ; "completion without a shell")]
#[test_case(&["completion", "tcsh"], "invalid value 'tcsh' for '<SHELL>'" ; "unknown shell")]
#[test_case(&["completion", "bash", "zsh"], "unexpected argument 'zsh'" ; "two shells")]
#[test_case(&["--config", "c.json", "completion", "bash"], SCOPE ; "leading config")]
#[test_case(&["--config=c.json", "completion", "bash"], SCOPE ; "leading attached config")]
#[test_case(&["completion", "bash", "--config", "c.json"], SCOPE ; "trailing config")]
#[test_case(&["--config", "c.json"], SCOPE ; "config without a command")]
fn usage_errors_exit_2_on_stderr_with_nothing_on_stdout(args: &[&str], text: &str) {
    let ran = togi(args);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(stdout(&ran), "");
    assert!(stderr(&ran).contains(text), "{}", stderr(&ran));
}

#[test]
fn a_missing_shell_error_names_the_argument() {
    let ran = togi(&["completion"]);
    assert!(stderr(&ran).contains("<SHELL>"), "{}", stderr(&ran));
    snapshot!(a_missing_shell_error_names_the_argument, stderr(&ran));
}

fn completion(shell: &str) -> String {
    let ran = togi(&["completion", shell]);
    assert_eq!(ran.status.code(), Some(0), "{shell}: {}", stderr(&ran));
    assert_eq!(stderr(&ran), "", "{shell}");
    stdout(&ran).to_owned()
}

#[test]
fn bash_completion() {
    snapshot!(bash_completion, completion("bash"));
}

#[test]
fn zsh_completion() {
    snapshot!(zsh_completion, completion("zsh"));
}

#[test]
fn fish_completion() {
    snapshot!(fish_completion, completion("fish"));
}

#[test]
fn nushell_completion() {
    snapshot!(nushell_completion, completion("nushell"));
}

#[test_case(&["--state-dir", "/state", "completion", "fish"] ; "state dir before the command")]
#[test_case(&["completion", "fish", "--state-dir", "/state"] ; "state dir after the command")]
fn completion_reads_and_writes_nothing_in_the_state_directory(args: &[&str]) {
    let host = support::Host::new();
    let ran = togi_on(&host, args);
    assert_eq!(ran.status.code(), Some(0), "{}", stderr(&ran));
    assert!(!stdout(&ran).is_empty());
    assert!(host.fs.kind(Path::new("/state")).unwrap().is_none());
    assert!(host.steps().is_empty());
}

#[test]
fn real_process_nushell_loads_the_script_without_warnings() {
    let nu = Command::new("nu").arg("--version").output();
    assert!(
        nu.is_ok(),
        "nu is not on PATH: the dev shell and the nextest checks provide it, and without it this test would pass without loading the script"
    );
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("togi.nu");
    std::fs::write(&script, completion("nushell")).unwrap();
    let ran = Command::new("nu")
        .args(["--no-config-file", "--no-history", "-c"])
        .arg(format!(
            "source '{}'; ['togi completion ' 'togi events --kind se'] | each {{|line| $line | commandline complete | str join ' '}} | str join (char newline)",
            script.display()
        ))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(0), "{}", stderr(&ran));
    assert_eq!(stderr(&ran), "");
    let lines: Vec<&str> = stdout(&ran).lines().collect();
    let [commands, kinds] = lines[..] else {
        panic!("two completions expected: {lines:?}")
    };
    let offered: Vec<&str> = commands.split_whitespace().collect();
    for shell in ["bash", "zsh", "fish", "nushell"] {
        assert!(offered.contains(&shell), "{shell} not offered: {offered:?}");
    }
    let offered: Vec<&str> = kinds.split_whitespace().collect();
    assert!(offered.contains(&"session"), "{offered:?}");
    assert!(offered.contains(&"session.start"), "{offered:?}");
}

/// Needs bash with the `compgen` builtin: the stdenv bash of a nix build has none, so the dev shell
/// and the nextest checks add `bashInteractive`.
fn bash_complete(word: &str) -> Vec<String> {
    let probe = Command::new("bash")
        .args(["-c", "type compgen"])
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "bash on PATH has no compgen builtin: use bashInteractive, which the dev shell provides"
    );
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("togi.bash");
    std::fs::write(&script, completion("bash")).unwrap();
    let ran = Command::new("bash")
        .args(["--norc", "--noprofile", "-c"])
        .arg(format!(
            "source '{}'; COMP_WORDS=(togi events --kind '{word}'); COMP_CWORD=3; _togi togi '{word}' --kind; printf '%s\\n' \"${{COMPREPLY[@]}}\"",
            script.display()
        ))
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(0), "{}", stderr(&ran));
    stdout(&ran)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn real_process_bash_completes_the_item_after_a_comma_in_events_kind() {
    assert_eq!(
        bash_complete("trial,cr"),
        ["trial,crash", "trial,crash.detected"]
    );
    assert_eq!(
        bash_complete("trial,checking.cycle,sess")
            .first()
            .map(String::as_str),
        Some("trial,checking.cycle,session")
    );
    assert_eq!(bash_complete("sess")[0], "session");
    assert!(bash_complete("trial,zzz").is_empty());
}

#[test]
fn run_help() {
    let ran = togi(&["run", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(stderr(&ran), "");
    snapshot!(run_help, stdout(&ran));
}

#[test]
fn the_cycles_example_is_worded_the_same_in_the_top_level_and_run_help() {
    let row = |text: &str| {
        let rows: Vec<String> = text
            .lines()
            .filter(|line| line.contains("sudo togi run --cycles 1"))
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        assert_eq!(rows.len(), 1, "{text}");
        rows.into_iter().next().unwrap()
    };
    let top = togi(&["--help"]);
    let run = togi(&["run", "--help"]);
    assert_eq!(row(stdout(&top)), row(stdout(&run)));
}

#[test_case(
    &["run", "--first-result", "--cycles", "1"],
    "cannot be used with" ;
    "first result with cycles"
)]
#[test_case(&["run", "--cycles", "0"], "must be a positive integer" ; "zero cycles")]
#[test_case(&["run", "--cycles", "many"], "must be a positive integer" ; "cycles not a number")]
#[test_case(&["run", "--cycles"], "a value is required" ; "cycles without a value")]
#[test_case(&["run", "--bogus"], "unexpected argument '--bogus'" ; "unknown flag")]
#[test_case(&["run", "--version"], "unexpected argument '--version'" ; "version")]
fn run_usage_errors_exit_2_before_anything_is_touched(args: &[&str], text: &str) {
    let host = support::Host::new();
    let state = "/state";
    let mut all = vec!["--state-dir", state];
    all.extend_from_slice(args);
    let ran = togi_on(&host, &all);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(stdout(&ran), "");
    assert!(stderr(&ran).contains(text), "{}", stderr(&ran));
    assert!(host.fs.kind(Path::new(state)).unwrap().is_none());
    assert!(host.steps().is_empty());
}

/// Config loading uses the injected host regardless of where the flag is written.
#[test_case(&["run", "--config", "/missing.json"] ; "after the command")]
#[test_case(&["--config", "/missing.json", "run"] ; "before the command")]
#[cfg(target_os = "linux")]
fn run_reads_its_config_wherever_the_flag_is_written(args: &[&str]) {
    let host = support::Host::new();
    let mut all = vec!["--state-dir", "/state"];
    all.extend(args.iter().copied());
    let ran = togi_on(&host, &all);
    assert_eq!(ran.status.code(), Some(2), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "");
    assert!(
        stderr(&ran).starts_with("togi run: cannot read config"),
        "{}",
        stderr(&ran)
    );
}

#[test]
#[cfg(target_os = "linux")]
fn run_with_an_invalid_config_is_a_usage_error() {
    let host = support::Host::new();
    let config = Path::new("/config.json");
    host.write(config, r#"{"bogus": 1}"#).unwrap();
    let ran = togi_on(
        &host,
        &[
            "run",
            "--state-dir",
            "/state",
            "--config",
            config.to_str().unwrap(),
        ],
    );
    assert_eq!(ran.status.code(), Some(2), "{}", stderr(&ran));
    assert!(
        stderr(&ran).starts_with("togi run: invalid config"),
        "{}",
        stderr(&ran)
    );
}

#[cfg(target_os = "linux")]
const NEWER_JOURNAL: &str = r#"{"seq":1,"time":"2026-01-01T00:00:00Z","boot":"b","mono_ms":0,"kind":"session.start","msg":"session started","data":{"build":{"version":"9.9.9","rev":"x","ruleset":1,"schema":1,"fixes":0,"evidence":2},"session":"20260101T000000Z","cores":[]}}
"#;

#[test]
#[cfg(target_os = "linux")]
fn run_refuses_a_journal_from_a_newer_evidence_epoch_before_the_config_and_appends_nothing() {
    let host = support::Host::new();
    let state = Path::new("/state");
    host.fs.create_dir(state).unwrap();
    host.write(state.join("events.jsonl"), NEWER_JOURNAL)
        .unwrap();
    let missing = Path::new("/missing.json");
    let ran = togi_on(
        &host,
        &[
            "run",
            "--state-dir",
            state.to_str().unwrap(),
            "--config",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!(ran.status.code(), Some(16), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "");
    assert!(
        stderr(&ran).starts_with(
            "togi run: this journal was written by togi 9.9.9+x (schema 1, ruleset 1, evidence epoch 2); this build, togi "
        ) && stderr(&ran).ends_with(
            ", uses evidence epoch 1. Install togi 9.9.9 to continue this session, or run togi reset --all to archive it and start over.\n"
        ),
        "{}",
        stderr(&ran)
    );
    assert_eq!(
        String::from_utf8(host.fs.read(&state.join("events.jsonl")).unwrap()).unwrap(),
        NEWER_JOURNAL
    );
}

fn help(command: &str) -> String {
    let ran = togi(&[command, "--help"]);
    assert_eq!(ran.status.code(), Some(0), "{command}");
    assert_eq!(stderr(&ran), "", "{command}");
    stdout(&ran).to_owned()
}

#[test]
fn reset_help() {
    snapshot!(reset_help, help("reset"));
}

#[test]
fn doctor_help() {
    snapshot!(doctor_help, help("doctor"));
}

#[test]
fn restart_limit_help() {
    snapshot!(restart_limit_help, help("restart-limit"));
}

#[test]
fn emergency_stop_help() {
    snapshot!(emergency_stop_help, help("emergency-stop"));
}

#[test]
fn night_help() {
    snapshot!(night_help, help("night"));
}

#[test]
fn night_end_help() {
    snapshot!(night_end_help, help("night-end"));
}

#[test_case("doctor" ; "doctor")]
#[test_case("reset" ; "reset")]
fn doctor_and_reset_list_config(command: &str) {
    assert!(help(command).contains("--config"));
}

#[test_case("restart-limit" ; "restart limit")]
#[test_case("emergency-stop" ; "emergency stop")]
fn restart_limit_and_emergency_stop_do_not_list_config(command: &str) {
    assert!(!help(command).contains("--config"));
}

#[test_case(&["reset"], "required arguments were not provided" ; "reset without a target")]
#[test_case(&["reset", "--core", "1", "--all"], "cannot be used with" ; "reset core and all")]
#[test_case(&["reset", "--core", "x"], "invalid value 'x'" ; "reset core not a number")]
#[test_case(&["reset", "--core", "-1"], "unexpected argument" ; "reset negative core")]
#[test_case(&["reset", "--all", "extra"], "unexpected argument 'extra'" ; "reset extra argument")]
#[test_case(&["restart-limit"], "required arguments were not provided" ; "restart limit without a boot")]
#[test_case(&["restart-limit", "--tuning-boot="], "a value is required" ; "restart limit empty boot")]
#[test_case(
    &["restart-limit", "--tuning-boot", "g", "extra"],
    "unexpected argument 'extra'" ;
    "restart limit extra argument"
)]
#[test_case(
    &["restart-limit", "--config", "c.json", "--tuning-boot", "g"],
    SCOPE ;
    "restart limit trailing config"
)]
#[test_case(
    &["--config", "c.json", "restart-limit", "--tuning-boot", "g"],
    SCOPE ;
    "restart limit leading config"
)]
#[test_case(&["emergency-stop", "extra"], "unexpected argument 'extra'" ; "emergency stop extra argument")]
#[test_case(
    &["emergency-stop", "--offset", "-5"],
    "unexpected argument '--offset'" ;
    "emergency stop offset"
)]
#[test_case(&["emergency-stop", "--config", "c.json"], SCOPE ; "emergency stop trailing config")]
#[test_case(&["--config", "c.json", "emergency-stop"], SCOPE ; "emergency stop leading config")]
#[test_case(&["doctor", "extra"], "unexpected argument 'extra'" ; "doctor extra argument")]
#[test_case(&["doctor", "--version"], "unexpected argument '--version'" ; "doctor version")]
fn administration_usage_errors_exit_2_before_touching_anything(args: &[&str], text: &str) {
    let host = support::Host::new();
    let state = "/state";
    let mut all = vec!["--state-dir", state];
    all.extend_from_slice(args);
    let ran = togi_on(&host, &all);
    assert_eq!(ran.status.code(), Some(2), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "");
    assert!(stderr(&ran).contains(text), "{}", stderr(&ran));
    assert!(host.fs.kind(Path::new(state)).unwrap().is_none());
    assert!(host.steps().is_empty(), "a usage error reached the host");
}

#[cfg(target_os = "linux")]
fn session_start(schema: u32) -> String {
    format!(
        "{{\"seq\":1,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"session.start\",\"msg\":\"x\",\"data\":{{\"build\":{{\"version\":\"9.9.9\",\"ruleset\":1,\"schema\":{schema},\"fixes\":0,\"evidence\":1}},\"session\":\"20260101T000000Z\",\"cores\":[]}}}}\n"
    )
}

#[test_case(&["--state-dir", "/state", "doctor"] ; "state dir before the command")]
#[test_case(&["doctor", "--state-dir", "/state"] ; "state dir after the command")]
#[cfg(target_os = "linux")]
fn doctor_refuses_a_newer_journal_with_exit_16_before_any_check(args: &[&str]) {
    let host = support::Host::new();
    let state = "/state";
    host.fs.create_dir(Path::new(state)).unwrap();
    host.write(Path::new(state).join("events.jsonl"), session_start(99))
        .unwrap();
    let ran = togi_on(&host, args);
    assert_eq!(ran.status.code(), Some(16), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "");
    assert!(
        stderr(&ran).starts_with("togi doctor: this journal was written by togi 9.9.9"),
        "{}",
        stderr(&ran)
    );
}

#[test]
#[cfg(target_os = "linux")]
fn doctor_exits_1_for_an_unreadable_journal_and_2_for_a_bad_config() {
    let host = support::Host::new();
    let state = "/state";
    host.fs.create_dir(Path::new(state)).unwrap();
    let missing = Path::new("/none.json");
    let ran = togi_on(
        &host,
        &[
            "--state-dir",
            state,
            "doctor",
            "--config",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!(ran.status.code(), Some(2), "{}", stderr(&ran));
    assert!(stderr(&ran).contains("none.json"), "{}", stderr(&ran));
    let ran = togi_on(
        &host,
        &[
            "--config",
            missing.to_str().unwrap(),
            "--state-dir",
            state,
            "doctor",
        ],
    );
    assert_eq!(ran.status.code(), Some(2), "{}", stderr(&ran));

    host.write(Path::new(state).join("events.jsonl"), "not json\n")
        .unwrap();
    let ran = togi_on(&host, &["--state-dir", state, "doctor"]);
    assert_eq!(ran.status.code(), Some(1), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "");
}
