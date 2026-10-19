use jiff::Timestamp;
use togi_watchtest::Scenario;

use crate::support::{
    BETWEEN_TRIAL_MCE, Journald, append_between_trial_mce, append_event, first_trial, lines,
    rewrite_first_line, session, text, togi, togi_in_journal,
};

#[test]
fn json_prints_the_journal_lines_unchanged() {
    let dir = session(Scenario::Search);
    let ran = togi(&dir, &["events", "--json"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    let printed: Vec<&str> = text(&ran.stdout).lines().collect();
    assert_eq!(printed, lines(&dir));
}

#[test_case::test_case(&[], true, "trial"; "trial")]
#[test_case::test_case(&["--core", "3", "--kind", "tuner.decision,core.phase"], false, "core"; "core")]
#[test_case::test_case(&["--kind", "session.start, session.baseline,"], false, "kinds"; "kinds")]
#[test_case::test_case(&["--kind", "session.start", "--kind", "session.baseline"], false, "kinds"; "repeated")]
#[test_case::test_case(&["--kind", "session", "--until", "2026-01-01T00:00:00.5Z"], false, "group"; "group")]
fn filters_narrow_the_readable_lines(extra: &[&str], select_trial: bool, golden: &str) {
    let dir = session(Scenario::Search);
    let trial = first_trial(&dir);
    let mut args = vec!["events"];
    args.extend_from_slice(extra);
    if select_trial {
        args.extend(["--trial", &trial]);
    }
    let ran = togi(&dir, &args);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    crate::support::snapshot(&format!("events__events_{golden}"), text(&ran.stdout));
}

#[test]
fn kind_filters_select_the_carried_facts_they_name() {
    use togi_journal::{Attribution, Failure, FailureCarried, Payload, TrialClass};
    use togi_machine::{Condition, Regime, Signal};

    use crate::status::{carried_pass, fact_source, installed, last_intent, then};

    let base = Scenario::Between.events().unwrap();
    let intent = last_intent(&base);
    let failure = FailureCarried {
        source: fact_source(&base, None),
        class: TrialClass {
            regime: Regime::R6,
            workload: None,
            cores: intent.cores,
            duration_s: 0,
        },
        failure: Failure {
            signal: Signal::Crash,
            attribution: Attribution::Unattributed,
            core: None,
            offset: None,
            trial: None,
            regime: Some(Regime::R6),
            condition: Some(Condition::Together),
            profile: Some(intent.profile),
            known_failure: None,
            reason: None,
            round: None,
        },
    };
    let carried: Vec<(Vec<_>, Payload)> = vec![
        (vec![], carried_pass(&base, &intent).into()),
        (vec![], failure.into()),
    ];
    let dir = installed(&then(&base, carried));
    let kinds_of = |filter: &str| -> Vec<String> {
        let ran = togi(&dir, &["events", "--kind", filter, "--json"]);
        assert_eq!(ran.status.code(), Some(0), "{filter}");
        text(&ran.stdout)
            .lines()
            .map(|line| {
                let event: serde_json::Value = serde_json::from_str(line).unwrap();
                event["kind"].as_str().unwrap().to_owned()
            })
            .collect()
    };
    let carried_only = |kinds: Vec<String>, wanted: &[&str]| {
        let carried: Vec<String> = kinds
            .into_iter()
            .filter(|kind| kind.ends_with(".carried"))
            .collect();
        assert_eq!(carried, wanted);
    };
    carried_only(kinds_of("trial"), &["trial.carried"]);
    carried_only(kinds_of("trial.carried"), &["trial.carried"]);
    carried_only(kinds_of("failure"), &["failure.carried"]);
    carried_only(kinds_of("failure.carried"), &["failure.carried"]);
    carried_only(
        kinds_of("trial,failure"),
        &["trial.carried", "failure.carried"],
    );
    assert_eq!(kinds_of("trial.carried"), ["trial.carried"]);
    assert_eq!(kinds_of("failure.carried"), ["failure.carried"]);
}

#[test]
fn a_time_window_is_inclusive_since_and_exclusive_until() {
    let dir = session(Scenario::Search);
    let all = togi(&dir, &["events", "--json"]);
    let times: Vec<Timestamp> = text(&all.stdout)
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            event["time"].as_str().unwrap().parse().unwrap()
        })
        .collect();
    let boundary = times[times.len() / 2];
    let text_of = boundary.to_string();
    let since = togi(&dir, &["events", "--json", "--since", &text_of]);
    let until = togi(&dir, &["events", "--json", "--until", &text_of]);
    assert_eq!(
        text(&since.stdout).lines().count(),
        times.iter().filter(|time| **time >= boundary).count()
    );
    assert_eq!(
        text(&until.stdout).lines().count(),
        times.iter().filter(|time| **time < boundary).count()
    );
}

#[test_case::test_case(&["events", "--trial", "no-such-trial"]; "trial")]
#[test_case::test_case(&["events", "--kind", "combination"]; "combination")]
#[test_case::test_case(&["events", "--kind", "trial", "--trial", "absent"]; "absent trial")]
fn a_valid_filter_with_no_match_prints_nothing_and_exits_zero(args: &[&str]) {
    let dir = session(Scenario::Search);
    let ran = togi(&dir, args);
    assert_eq!(ran.status.code(), Some(0), "{args:?}");
    assert_eq!(text(&ran.stdout), "", "{args:?}");
    assert_eq!(text(&ran.stderr), "", "{args:?}");
}

#[test_case::test_case("no.such"; "unknown name")]
#[test_case::test_case("nosuchgroup"; "unknown group")]
#[test_case::test_case(","; "comma")]
#[test_case::test_case(" "; "whitespace")]
#[test_case::test_case(""; "empty")]
fn kind_errors_are_usage_errors_before_the_journal_is_read(kind: &str) {
    let empty = crate::support::Session::empty();
    let ran = togi(&empty, &["events", "--kind", kind]);
    assert_eq!(ran.status.code(), Some(2), "{kind:?}");
    assert_eq!(text(&ran.stdout), "", "{kind:?}");
    assert!(
        text(&ran.stderr).contains("valid names:"),
        "{kind:?}: {}",
        text(&ran.stderr)
    );
    assert!(text(&ran.stderr).contains("trial.intent"), "{kind:?}");
    let ran = togi(&empty, &["events", "--kind", "trial", "--kind", ","]);
    assert_eq!(ran.status.code(), Some(2));
}

#[test_case::test_case(&["events", "--since", "yesterday"]; "since")]
#[test_case::test_case(&["events", "--until", "2026-01-01"]; "until")]
#[test_case::test_case(&["events", "--core", "x"]; "core text")]
#[test_case::test_case(&["events", "--core", "999"]; "core range")]
#[test_case::test_case(&["events", "extra"]; "positional")]
fn flag_value_errors_are_usage_errors(args: &[&str]) {
    let empty = crate::support::Session::empty();
    let ran = togi(&empty, args);
    assert_eq!(ran.status.code(), Some(2), "{args:?}");
    assert_eq!(text(&ran.stdout), "");
}

#[test]
fn a_missing_journal_is_an_error_naming_the_file() {
    let empty = crate::support::Session::empty();
    let ran = togi(&empty, &["events"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).contains("no journal at"),
        "{}",
        text(&ran.stderr)
    );
    assert!(text(&ran.stderr).contains("events.jsonl"));
}

#[test]
fn a_file_that_is_not_a_journal_is_an_error() {
    let dir = crate::support::Session::empty();
    crate::support::write_journal(&dir, "not json\n");
    let ran = togi(&dir, &["events"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).starts_with("togi events:"),
        "{}",
        text(&ran.stderr)
    );
}

#[test_case::test_case("events"; "events")]
#[test_case::test_case("status"; "status")]
fn a_newer_schema_is_refused_with_advice(command: &str) {
    let dir = session(Scenario::Search);
    rewrite_first_line(&dir, |event| {
        event["data"]["build"]["schema"] = 99.into();
    });
    let ran = togi(&dir, &[command]);
    assert_eq!(ran.status.code(), Some(1), "{command}");
    assert_eq!(text(&ran.stdout), "", "{command}");
    let err = text(&ran.stderr);
    assert!(err.contains("schema 99"), "{err}");
    assert!(err.contains("togi reset --all"), "{err}");
}

#[test]
fn an_older_schema_is_refused_too() {
    let dir = session(Scenario::Search);
    rewrite_first_line(&dir, |event| {
        event["data"]["build"]["schema"] = 0.into();
    });
    let ran = togi(&dir, &["events"]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        text(&ran.stderr).contains("uses schema 1"),
        "{}",
        text(&ran.stderr)
    );
}

#[test_case::test_case(&["events", "--json"]; "json")]
#[test_case::test_case(&["events", "--kind=session"]; "session")]
#[test_case::test_case(&["status"]; "status")]
fn a_different_ruleset_warns_and_still_renders(args: &[&str]) {
    let dir = session(Scenario::Search);
    rewrite_first_line(&dir, |event| {
        event["data"]["build"]["ruleset"] = 7.into();
    });
    let ran = togi(&dir, &args);
    assert_eq!(ran.status.code(), Some(0), "{args:?}");
    assert!(
        text(&ran.stderr).contains("ruleset is 7"),
        "{}",
        text(&ran.stderr)
    );
    assert!(!text(&ran.stdout).is_empty());
}

#[test_case::test_case("events"; "events")]
#[test_case::test_case("status"; "status")]
fn a_torn_tail_is_reported_after_the_events(command: &str) {
    let dir = session(Scenario::Search);
    let whole = lines(&dir).len();
    crate::support::append_bytes(&dir, b"{\"seq\":");
    let ran = togi(&dir, &[command]);
    assert_eq!(ran.status.code(), Some(0), "{command}");
    assert!(
        text(&ran.stderr).contains("journal ends with 7 torn bytes"),
        "{}",
        text(&ran.stderr)
    );
    if command == "events" {
        assert_eq!(text(&ran.stdout).lines().count(), whole);
    }
    assert_eq!(
        lines(&dir).len(),
        whole + 1,
        "reading never repairs the tail"
    );
}

#[test]
fn a_single_torn_byte_is_reported_in_the_singular() {
    let dir = session(Scenario::Search);
    crate::support::append_bytes(&dir, b"{");
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(0));
    let stderr = text(&ran.stderr);
    assert!(
        stderr.contains("journal ends with 1 torn byte;"),
        "{stderr}"
    );
    assert!(!stderr.contains("1 torn bytes"), "{stderr}");
}

#[test]
fn state_dir_works_before_and_after_the_command() {
    let dir = session(Scenario::Search);
    let path = dir.path().to_str().unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = togi_cli::run_with(
        ["togi", "events", "--json", "--state-dir", path],
        &mut stdout,
        &mut stderr,
        &dir.host(),
    );
    let before = togi(&dir, &["events", "--json"]);
    assert_eq!(exit, togi_cli::Exit::Ok);
    assert_eq!(stderr, before.stderr);
    assert_eq!(stdout, before.stdout);
}

#[test]
fn events_help() {
    let ran = togi(&crate::support::Session::empty(), &["events", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    crate::support::snapshot("events__events_help", text(&ran.stdout));
}

#[test]
fn a_between_trial_mce_is_listed_and_its_controls_are_escaped() {
    let dir = session(Scenario::Search);
    append_between_trial_mce(&dir);
    let ran = togi(&dir, &["events", "--kind", "mce"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    let shown = text(&ran.stdout);
    assert_eq!(shown.lines().count(), 1, "{shown}");
    assert!(shown.contains("between trials (recorded only)"), "{shown}");
    assert!(shown.contains("\\x1b[2J"), "{shown}");
    assert!(!shown.contains('\u{1b}'), "{shown}");

    let raw = togi(&dir, &["events", "--kind", "mce", "--json"]);
    let event: serde_json::Value = serde_json::from_str(text(&raw.stdout).trim()).unwrap();
    assert_eq!(
        event["msg"], BETWEEN_TRIAL_MCE,
        "--json keeps the journal's text"
    );
}

#[test]
fn human_lines_are_styled_for_the_system_journal_and_json_is_not() {
    let dir = session(Scenario::Search);
    append_event(
        &dir,
        "failure",
        "crash",
        &serde_json::json!({"signal": "crash", "attribution": "attributed"}),
    );
    let human = togi_in_journal(&dir, &["events", "--kind", "failure"], Journald::Stdout);
    assert_eq!(human.status.code(), Some(0), "{}", text(&human.stderr));
    let shown = text(&human.stdout);
    assert_eq!(shown.lines().count(), 1, "{shown:?}");
    assert!(shown.starts_with("<3>\x1b[31m"), "{shown:?}");
    assert!(shown.ends_with("\x1b[0m\n"), "{shown:?}");

    let raw = togi_in_journal(
        &dir,
        &["events", "--kind", "failure", "--json"],
        Journald::Stdout,
    );
    assert_eq!(raw.status.code(), Some(0), "{}", text(&raw.stderr));
    let json = text(&raw.stdout);
    assert!(!json.contains('\x1b') && !json.contains("<3>"), "{json:?}");
    let printed: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let recorded: serde_json::Value = serde_json::from_str(lines(&dir).last().unwrap()).unwrap();
    assert_eq!(printed, recorded);
}
