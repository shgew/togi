#![allow(
    clippy::unwrap_used,
    reason = "the helpers panic on a broken fixture, as the tests do"
)]

#[path = "../support/mod.rs"]
mod support;

use std::path::Path;
use test_case::test_case;

use support::{Host, Ran, Session, files};
use togi_cli::Exit;
use togi_cli::system::System;
use togi_journal::fs::{Fault, Fs, Op};
use togi_machine::{CoreId, Signal};
use togi_sim::model::Script;
use togi_tuner::RULESET;
use togi_watchtest::{Scenario, Stop, default_machine, install_events_with_fs, simulate};

const SESSION: &str = "20260101T000000Z";

fn reset(host: &Host, state: &Path, args: &[&str]) -> Ran {
    let mut all = vec!["reset"];
    all.extend_from_slice(args);
    host.run(state, &all)
}

fn archive(state: &Path, name: &str) -> std::path::PathBuf {
    state.join("archive").join(name)
}

fn last_event(session: &Session) -> serde_json::Value {
    serde_json::from_str(session.text().lines().last().unwrap()).unwrap()
}

#[test]
fn reset_core_queues_one_command_in_the_current_boot() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let before = session.kinds();
    let ran = reset(&host, session.path(), &["--core", "3"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        "reset of core 03 queued; the next togi run restarts its search from the baseline\n"
    );
    assert_eq!(ran.err, "00:00:00 command.reset  reset core 03\n");
    let after = session.kinds();
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(&after[before.len()..], ["command.reset"]);
    let event = last_event(&session);
    assert_eq!(
        event["data"],
        serde_json::json!({"target": {"core": 3}}),
        "{event}"
    );
    assert_eq!(event["boot"], host.boot.as_str());
}

#[test_case("16"; "core 16")]
#[test_case("99"; "core 99")]
fn reset_core_of_a_core_the_session_lacks_exits_2_and_changes_nothing(core: &str) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let before = session.text();
    let ran = reset(&host, session.path(), &["--core", core]);
    assert_eq!(ran.exit, Exit::Usage, "{core}: {}", ran.err);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        format!("togi reset: reset core {core}: no such core\n")
    );
    assert_eq!(session.text(), before, "{core}");
}

#[test_case(&[]; "no target")]
#[test_case(&["--core", "1", "--all"]; "both targets")]
#[test_case(&["--core", "x"]; "not numeric")]
#[test_case(&["--core", "-1"]; "negative")]
#[test_case(&["--core", "300"]; "out of range")]
fn exactly_one_of_core_and_all_and_a_core_number(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let before = files(&host.fs, session.path());
    let ran = reset(&host, session.path(), args);
    assert_eq!(ran.exit, Exit::Usage, "{args:?}: {}", ran.err);
    assert_eq!(ran.out, "", "{args:?}");
    assert_eq!(files(&host.fs, session.path()), before, "{args:?}");
    assert_eq!(
        host.lock_acquisitions.get(),
        0,
        "{args:?}: the host lock was taken"
    );
}

#[test_case(&["--core", "0"]; "core")]
#[test_case(&["--all"]; "all")]
fn no_journal_or_no_session_exits_1_without_creating_state(args: &[&str]) {
    let host = Host::new();
    let state = support::MemoryDir::new();
    let ran = reset(&host, state.path(), args);
    assert_eq!(ran.exit, Exit::Error, "{args:?}");
    assert!(ran.err.contains("no journal at"), "{}", ran.err);
    assert_eq!(
        files(&host.fs, state.path()),
        Vec::<String>::new(),
        "{args:?}"
    );

    let content = "";
    host.write(state.path().join("events.jsonl"), content)
        .unwrap();
    let before = files(&host.fs, state.path());
    let ran = reset(&host, state.path(), args);
    assert_eq!(ran.exit, Exit::Error, "{args:?}");
    assert_eq!(
        ran.err,
        format!("togi reset: no session in {}\n", state.path().display())
    );
    assert_eq!(
        files(&host.fs, state.path()),
        before,
        "{args:?}: {content:?}"
    );
    assert_eq!(
        host.read_to_string(state.path().join("events.jsonl"))
            .unwrap(),
        content,
        "{args:?}"
    );
}

#[test_case(&["--core", "0"]; "core")]
#[test_case(&["--all"]; "all")]
fn a_malformed_first_line_is_refused_as_ambiguous_without_changing_state(args: &[&str]) {
    let host = Host::new();
    let state = support::MemoryDir::new();
    let content = "{\"seq\":";
    host.write(state.path().join("events.jsonl"), content)
        .unwrap();
    let before = files(&host.fs, state.path());
    let ran = reset(&host, state.path(), args);
    assert_eq!(ran.exit, Exit::Error, "{args:?}");
    assert_eq!(ran.out, "", "{args:?}");
    assert_eq!(
        ran.err,
        format!(
            "togi reset: {}: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover\n",
            state.path().join("events.jsonl").display()
        ),
        "{args:?}"
    );
    assert_eq!(
        files(&host.fs, state.path()),
        before,
        "{args:?}: no lock file"
    );
    assert_eq!(
        host.read_to_string(state.path().join("events.jsonl"))
            .unwrap(),
        content,
        "{args:?}"
    );
}

#[test]
fn reset_all_archives_the_session_and_marks_the_boundary() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let original = session.text();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!(
            "session {SESSION} archived to archive/{SESSION}.jsonl; the next togi run starts a new session\n"
        )
    );
    let logged: Vec<&str> = ran.err.lines().collect();
    assert_eq!(logged.len(), 2, "{}", ran.err);
    assert!(
        logged[0].starts_with("00:00:00 command.reset "),
        "{}",
        ran.err
    );
    assert!(
        logged[1].starts_with("00:00:00 session.archived "),
        "{}",
        ran.err
    );
    assert!(!host.exists(session.journal()));
    let archived = host
        .read_to_string(archive(session.path(), &format!("{SESSION}.jsonl")))
        .unwrap();
    assert!(archived.starts_with(&original));
    let kinds: Vec<&str> = archived
        .lines()
        .map(|line| {
            let kind = line.split("\"kind\":\"").nth(1).unwrap();
            kind.split('"').next().unwrap()
        })
        .collect();
    assert_eq!(
        &kinds[kinds.len() - 2..],
        ["command.reset", "session.archived"]
    );
    assert!(host.exists(archive(session.path(), &format!("{SESSION}-reset-all"))));
}

#[test]
fn reset_all_of_an_unrecognized_schema_archives_it_unread() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let original = session.text();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!(
            "session {SESSION} archived to archive/{SESSION}.jsonl without appending to the incompatible journal; the next togi run starts a new session\n"
        )
    );
    assert_eq!(
        host.read_to_string(archive(session.path(), &format!("{SESSION}.jsonl")))
            .unwrap(),
        original
    );
    assert!(host.exists(archive(session.path(), &format!("{SESSION}-reset-all"))));
    assert!(!host.exists(session.journal()));
}

#[test]
fn reset_all_of_another_ruleset_archives_it_normally() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    session.rewrite_first_line(&format!("\"ruleset\":{RULESET}"), "\"ruleset\":99");
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.out.contains("without appending"), "{}", ran.out);
    let archived = host
        .read_to_string(archive(session.path(), &format!("{SESSION}.jsonl")))
        .unwrap();
    assert!(archived.contains("\"kind\":\"command.reset\""));
}

#[test_case(Some(99), None, None, true; "newer schema")]
#[test_case(Some(0), None, None, false; "older schema")]
#[test_case(None, Some(99), None, true; "newer ruleset")]
#[test_case(None, None, Some(99), true; "newer evidence")]
#[test_case(None, Some(0), None, false; "older ruleset")]
#[test_case(None, Some(0), Some(99), true; "older ruleset with newer evidence")]
fn reset_core_refuses_every_stamp_mismatch_with_exit_16_and_appends_nothing(
    schema: Option<u32>,
    ruleset: Option<u32>,
    evidence: Option<u32>,
    install_required: bool,
) {
    let archives = "togi run archives it and starts a new session that carries its candidate solo limits and failure points; togi reset --all archives it and starts over.";
    let install = "Install togi 0.0.0 to continue this session, or run togi reset --all to archive it and start over.";
    let advice = if install_required { install } else { archives };
    let (written, uses) = if let Some(schema) = schema {
        (
            format!("(schema {schema}, ruleset {RULESET})"),
            "schema 1".to_owned(),
        )
    } else if let Some(ruleset) = ruleset {
        (
            format!("(schema 1, ruleset {ruleset})"),
            format!("ruleset {RULESET}"),
        )
    } else {
        (
            format!("(schema 1, ruleset {RULESET}, evidence epoch 99)"),
            "evidence epoch 1".to_owned(),
        )
    };
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    if let Some(schema) = schema {
        session.rewrite_first_line("\"schema\":1", &format!("\"schema\":{schema}"));
    }
    if let Some(ruleset) = ruleset {
        session.rewrite_first_line(
            &format!("\"ruleset\":{RULESET}"),
            &format!("\"ruleset\":{ruleset}"),
        );
    }
    if let Some(evidence) = evidence {
        session.rewrite_first_line("\"evidence\":1}", &format!("\"evidence\":{evidence}}}"));
    }
    session.rewrite_first_line(
        "\"version\":\"0.0.0\"",
        "\"version\":\"0.0.0\",\"rev\":\"abc1234\"",
    );
    let before = session.text();
    let ran = reset(&host, session.path(), &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert!(
            ran.err.starts_with(&format!(
                "togi reset: this journal was written by togi 0.0.0+abc1234 {written}; this build, togi "
            )),
            "{}",
            ran.err
        );
    assert!(
        ran.err.ends_with(&format!(", uses {uses}. {advice}\n")),
        "{}",
        ran.err
    );
    assert_eq!(session.text(), before);
}

#[test_case(&["--core", "0"]; "core")]
#[test_case(&["--all"]; "all")]
fn an_unreadable_journal_exits_1(args: &[&str]) {
    let host = Host::new();
    let state = support::MemoryDir::new();
    host.write(state.path().join("events.jsonl"), "not json\n")
        .unwrap();
    let ran = reset(&host, state.path(), args);
    assert_eq!(ran.exit, Exit::Error, "{args:?}: {}", ran.err);
    assert_eq!(ran.out, "");
}

#[test_case(&["--core", "0"]; "core")]
#[test_case(&["--all"]; "all")]
fn a_busy_host_lock_exits_3_naming_it_before_anything_changes(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let before = files(&host.fs, session.path());
    let text = session.text();
    let _held = host.locks.acquire(&host.lock).unwrap();
    let ran = reset(&host, session.path(), args);
    assert_eq!(ran.exit, Exit::Locked, "{args:?}: {}", ran.err);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        format!(
            "togi reset: acquire host lock {}: another togi process holds the host lock\n",
            host.lock.display()
        )
    );
    assert_eq!(files(&host.fs, session.path()), before);
    assert_eq!(session.text(), text);
}

#[test]
fn a_host_lock_that_cannot_be_taken_exits_1_with_its_cause_before_anything_changes() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let before = files(&host.fs, session.path());
    *host.lock_error.borrow_mut() = Some(togi_hostlock::Error::Io {
        path: host.lock.clone(),
        operation: togi_hostlock::Operation::Lock,
        source: std::io::Error::other("disk gone"),
    });
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        format!(
            "togi reset: lock host lock {}: disk gone\n",
            host.lock.display()
        )
    );
    assert_eq!(files(&host.fs, session.path()), before);
}

#[test_case(&["--core", "0"]; "core")]
#[test_case(&["--all"]; "all")]
fn a_held_state_directory_lock_exits_3_and_changes_nothing(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let text = session.text();
    let held = host.lock_state_dir(session.path()).unwrap();
    let ran = reset(&host, session.path(), args);
    assert_eq!(ran.exit, Exit::Locked, "{args:?}: {}", ran.err);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        "togi reset: another togi process holds the journal lock\n"
    );
    drop(held);
    assert_eq!(session.text(), text);
}

#[test]
fn a_reset_journal_lock_failure_prints_its_path_and_cause_once() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    host.fs.inject(Fault::before(Op::Open, 1).named("lock"));
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        format!(
            "togi reset: lock {}: injected Open fault\n",
            session.path().join("lock").display()
        )
    );
}

#[test]
fn a_pending_carry_is_dropped_without_a_journal() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    host.create_dir_all(state.path().join("archive")).unwrap();
    host.write(
        archive(state.path(), &format!("{SESSION}-carry-pending")),
        "",
    )
    .unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!(
            "carry from session {SESSION} dropped; the next togi run starts a new session with nothing carried\n"
        )
    );
    assert!(!host.exists(archive(state.path(), &format!("{SESSION}-carry-pending"))));
    assert!(host.exists(archive(state.path(), &format!("{SESSION}-reset-all"))));
}

#[test]
fn reset_all_completes_an_interrupted_incompatible_archive() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    host.create_dir_all(state.path().join("archive")).unwrap();
    host.write(
        archive(state.path(), &format!("{SESSION}.jsonl")),
        "moved\n",
    )
    .unwrap();
    host.write(
        archive(state.path(), &format!("{SESSION}-compat-pending")),
        "",
    )
    .unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!(
            "session {SESSION} archived to archive/{SESSION}.jsonl without appending to the incompatible journal; the next togi run starts a new session\n"
        )
    );
    assert!(!host.exists(archive(state.path(), &format!("{SESSION}-compat-pending"))));
    assert!(host.exists(archive(state.path(), &format!("{SESSION}-reset-all"))));
    assert!(
        !host.exists(state.path().join("events.jsonl")),
        "reset recreated the journal"
    );
    assert_eq!(
        host.read_to_string(archive(state.path(), &format!("{SESSION}.jsonl")))
            .unwrap(),
        "moved\n"
    );
}

fn failing_scenario() -> (Scenario, CoreId, i64) {
    for scenario in [Scenario::Deadend, Scenario::Hunt] {
        let events = scenario.events().unwrap();
        let state = togi_tuner::replay(&events).unwrap();
        let points = state.constraints().unwrap().failure_points();
        if let Some((core, point)) = points.iter().next() {
            return (scenario, core, point.offset().get());
        }
    }
    panic!("no scenario records a failure point");
}

#[test_case(0, true, false; "at failure point replay")]
#[test_case(-1, true, false; "below failure point replay")]
#[test_case(1, false, false; "above failure point replay")]
#[test_case(0, true, true; "at failure point snapshot")]
#[test_case(-1, true, true; "below failure point snapshot")]
#[test_case(1, false, true; "above failure point snapshot")]
fn reset_all_warns_for_each_solo_limit_at_or_past_its_failure_point(
    delta: i64,
    warns: bool,
    snapshot: bool,
) {
    let (scenario, core, point) = failing_scenario();
    let core = core.get();
    assert!(point < 0, "this case pins the remedy that offers a limit");
    let host = Host::new();
    let cfg = support::MemoryDir::new();
    let write = |limit: i64| {
        let path = cfg.path().join(format!("config{limit}.json"));
        host.write(
            &path,
            format!(r#"{{"candidate_solo_limits":{{"{core}":{limit}}}}}"#),
        )
        .unwrap();
        path
    };
    let limit = point + delta;
    if !(-50..=0).contains(&limit) {
        return;
    }
    let session = Session::with_fs(host.fs.clone(), scenario);
    if snapshot {
        let events = scenario.events().unwrap();
        let cut = events.len() / 2;
        let folded = togi_session::ReadState::replay(&events[..cut]).unwrap();
        let bytes: Vec<u8> = events[..cut]
            .iter()
            .flat_map(|event| {
                let mut line = event.encode().unwrap().into_bytes();
                line.push(b'\n');
                line
            })
            .collect();
        let prefix = togi_journal::snapshot::Prefix::of(&bytes, Some(events[cut - 1].seq));
        let saved = togi_journal::snapshot::seal(prefix, &folded.document()).unwrap();
        host.fs.put(
            session.path().join("state.json"),
            serde_json::to_vec(&saved).unwrap(),
        );
    }
    let path = write(limit);
    let ran = reset(
        &host,
        session.path(),
        &["--all", "--config", path.to_str().unwrap()],
    );
    assert_eq!(ran.exit, Exit::Ok, "{limit}: {}", ran.err);
    let want = format!(
        "warning: candidate solo limit {limit} for core {core:02} is at or deeper than its failure point {point} in the archived session; "
    );
    assert_eq!(ran.err.contains(&want), warns, "{limit}: {}", ran.err);
    if warns {
        let remedy = format!(
            "use {}, the failure point plus one, or remove it",
            point + 1
        );
        assert!(
            ran.err.contains(&format!("{want}{remedy}\n")),
            "{}",
            ran.err
        );
    }
}

#[test_case(true; "malformed")]
#[test_case(false; "missing")]
fn an_invalid_or_missing_config_never_stops_the_archive(malformed: bool) {
    let host = Host::new();
    let cfg = support::MemoryDir::new();
    let bad = cfg.path().join("bad.json");
    host.write(&bad, "{").unwrap();
    let missing = cfg.path().join("missing.json");
    let path = if malformed { &bad } else { &missing };
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let ran = reset(
        &host,
        session.path(),
        &["--all", "--config", path.to_str().unwrap()],
    );
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(
        ran.err
            .contains("warning: cannot check candidate solo limits: "),
        "{}",
        ran.err
    );
    assert!(!host.exists(session.journal()));
}

#[test]
fn an_invalid_explicit_config_warning_keeps_the_parser_cause() {
    let host = Host::new();
    let cfg = support::MemoryDir::new();
    let path = cfg.path().join("bad.json");
    host.write(&path, "{").unwrap();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let ran = reset(
        &host,
        session.path(),
        &["--all", "--config", path.to_str().unwrap()],
    );
    assert_eq!(ran.exit, Exit::Ok);
    assert_eq!(ran.err.lines().find(|line| line.starts_with("warning:")),
        Some(format!("warning: cannot check candidate solo limits: invalid config {}: parse config JSON: EOF while parsing an object at line 1 column 1", path.display()).as_str()));
    assert!(!host.exists(session.journal()));
}

#[test_case(true; "config first")]
#[test_case(false; "config last")]
fn config_is_accepted_before_and_after_the_command(config_first: bool) {
    let host = Host::new();
    let cfg = support::MemoryDir::new();
    let missing = cfg.path().join("missing.json");
    let missing = missing.to_str().unwrap();
    let args = if config_first {
        &["--config", missing, "reset", "--all"][..]
    } else {
        &["reset", "--all", "--config", missing][..]
    };
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let ran = host.run(session.path(), args);
    assert_eq!(ran.exit, Exit::Ok, "{args:?}: {}", ran.err);
    assert!(
        ran.err.contains("cannot check candidate solo limits"),
        "{args:?}: {}",
        ran.err
    );
}

#[test]
fn error_text_escapes_control_characters() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    let nasty = state.path().join("a\x1b[31mb");
    let ran = reset(&host, &nasty, &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(!ran.err.contains('\x1b'), "{:?}", ran.err);
}

#[test]
fn a_held_writer_lock_leaves_a_pending_carry_alone() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    host.create_dir_all(state.path().join("archive")).unwrap();
    let marker = archive(state.path(), &format!("{SESSION}-carry-pending"));
    host.write(&marker, "").unwrap();
    let held = host.lock_state_dir(state.path()).unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    drop(held);
    assert!(host.exists(&marker));
    assert!(!host.exists(archive(state.path(), &format!("{SESSION}-reset-all"))));
}

#[test]
fn pending_markers_are_found_in_a_state_directory_named_like_a_pattern() {
    let host = Host::new();
    let parent = support::MemoryDir::new();
    let state = parent.path().join("state[1]*?{a,b}");
    host.create_dir_all(state.join("archive")).unwrap();
    host.write(archive(&state, &format!("{SESSION}-carry-pending")), "")
        .unwrap();
    let ran = reset(&host, &state, &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.out.starts_with("carry from session"), "{}", ran.out);
}

#[test]
fn an_incompatible_archive_refuses_a_collision_and_resumes_a_pending_move() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let original = session.text();
    host.create_dir_all(session.path().join("archive")).unwrap();
    let target = archive(session.path(), &format!("{SESSION}.jsonl"));
    host.write(&target, "taken\n").unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(session.text(), original);
    assert_eq!(host.read_to_string(&target).unwrap(), "taken\n");

    host.fs.remove_file(&target).unwrap();
    host.write(
        archive(session.path(), &format!("{SESSION}-compat-pending")),
        "",
    )
    .unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(host.read_to_string(&target).unwrap(), original);
    assert!(!host.exists(session.journal()));
    assert!(!host.exists(archive(
        session.path(),
        &format!("{SESSION}-compat-pending")
    )));
}

#[test]
fn a_journal_that_already_records_its_archive_is_only_moved() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let mut text = session.text();
    text = format!(
        "{text}{{\"seq\":9999,\"kind\":\"session.archived\",\"data\":{{\"session\":\"{SESSION}\"}}}}\n"
    );
    host.write(session.journal(), &text).unwrap();
    host.create_dir_all(session.path().join("archive")).unwrap();
    host.write(
        archive(session.path(), &format!("{SESSION}-trials")),
        "kept\n",
    )
    .unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        host.read_to_string(archive(session.path(), &format!("{SESSION}.jsonl")))
            .unwrap(),
        text
    );
    assert_eq!(
        host.read_to_string(archive(session.path(), &format!("{SESSION}-trials")))
            .unwrap(),
        "kept\n"
    );
    assert!(host.exists(archive(session.path(), &format!("{SESSION}-reset-all"))));
}

#[test_case(&["--core", "0"]; "core")]
#[test_case(&["--all"]; "all")]
fn a_current_journal_with_an_unknown_kind_exits_16_and_is_left_alone(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let mut text = session.text();
    text = format!(
        "{text}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"future.fact\",\"msg\":\"x\",\"data\":{{}}}}\n",
        session.kinds().len() + 1
    );
    host.write(session.journal(), &text).unwrap();
    let ran = reset(&host, session.path(), args);
    assert_eq!(ran.exit, Exit::Incompatible, "{args:?}: {}", ran.err);
    assert!(
            ran.err.starts_with(&format!(
                "togi reset: unknown kind \"future.fact\" in journal written by togi 0.0.0 (schema 1, ruleset {RULESET}); this build, togi "
            )),
            "{args:?}: {}",
            ran.err
        );
    assert!(
            ran.err.ends_with(&format!(
                " (schema 1, ruleset {RULESET}), cannot safely modify it; install the build that wrote it\n"
            )),
            "{args:?}: {}",
            ran.err
        );
    assert!(!ran.err.contains("reset --all"), "{args:?}: {}", ran.err);
    assert_eq!(session.text(), text, "{args:?}");
    assert!(!host.exists(session.path().join("archive")), "{args:?}");
}

#[test]
fn a_hostile_unknown_kind_is_escaped_in_the_refusal_and_the_journal_is_left_alone() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let mut text = session.text();
    text = format!(
        "{text}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"future.\\u001b[2J\\u202e\\nforged\",\"msg\":\"x\",\"data\":{{}}}}\n",
        session.kinds().len() + 1
    );
    host.write(session.journal(), &text).unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(
        ran.err.starts_with("togi reset: unknown kind \"future."),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("forged"), "{}", ran.err);
    assert!(
        !ran.err.contains(['\u{1b}', '\u{202e}']),
        "a control reached the terminal: {:?}",
        ran.err
    );
    assert_eq!(
        ran.err.matches('\n').count(),
        1,
        "the refusal is one line: {:?}",
        ran.err
    );
    assert_eq!(session.text(), text);
    assert!(!host.exists(session.path().join("archive")));
}

#[test_case(&["--core", "0"]; "core")]
#[test_case(&["--all"]; "all")]
fn a_current_journal_with_an_unknown_field_exits_16_and_is_left_alone(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let mut text = session.text();
    text = format!(
        "{text}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"session.warning\",\"msg\":\"x\",\"data\":{{\"operation\":\"x\",\"error\":\"y\",\"future_field\":1}}}}\n",
        session.kinds().len() + 1
    );
    host.write(session.journal(), &text).unwrap();
    let ran = reset(&host, session.path(), args);
    assert_eq!(ran.exit, Exit::Incompatible, "{args:?}: {}", ran.err);
    assert!(
            ran.err.starts_with(&format!(
                "togi reset: unknown field \"data.future_field\" of a session.warning event in journal written by togi 0.0.0 (schema 1, ruleset {RULESET}); this build, togi "
            )),
            "{args:?}: {}",
            ran.err
        );
    assert!(
            ran.err.ends_with(&format!(
                " (schema 1, ruleset {RULESET}), cannot safely modify it; install the build that wrote it\n"
            )),
            "{args:?}: {}",
            ran.err
        );
    assert_eq!(session.text(), text, "{args:?}");
    assert!(!host.exists(session.path().join("archive")), "{args:?}");
}

#[test]
fn solo_limits_are_checked_across_rulesets_and_skipped_across_schemas() {
    let (scenario, core, point) = failing_scenario();
    let host = Host::new();
    let cfg = support::MemoryDir::new();
    let path = cfg.path().join("config.json");
    host.write(
        &path,
        format!(
            r#"{{"candidate_solo_limits":{{"{}":{}}}}}"#,
            core.get(),
            point
        ),
    )
    .unwrap();
    let path = path.to_str().unwrap();
    let ruleset = Session::with_fs(host.fs.clone(), scenario);
    ruleset.rewrite_first_line(&format!("\"ruleset\":{RULESET}"), "\"ruleset\":99");
    let ran = reset(&host, ruleset.path(), &["--all", "--config", path]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.err.contains("candidate solo limit"), "{}", ran.err);

    let schema = Session::with_fs(host.fs.clone(), scenario);
    schema.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let ran = reset(&host, schema.path(), &["--all", "--config", path]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.err, "",
        "a journal of another schema is archived without replay"
    );
}

#[test]
fn a_missing_default_config_is_silent() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.err.contains("warning"), "{}", ran.err);
}

#[test]
fn real_fs_a_host_lock_that_cannot_be_opened_exits_1() {
    let mut host = Host::real();
    host.lock = host
        .lock
        .parent()
        .unwrap()
        .join("missing")
        .join("togi.lock");
    let ran = reset(&host, &host.lock.with_file_name("state"), &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
}

#[test]
fn infrastructure_failures_exit_1() {
    let mut host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    host.boot_fails = true;
    let ran = reset(&host, session.path(), &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);

    let host = session.host();
    let file = session.path().join("events.jsonl");
    let ran = reset(&host, &file, &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
}

#[test]
fn the_default_config_comes_from_the_host() {
    let (scenario, core, point) = failing_scenario();
    let host = Host::new();
    host.write(
        &host.config,
        format!(
            r#"{{"candidate_solo_limits":{{"{}":{point}}}}}"#,
            core.get()
        ),
    )
    .unwrap();
    let session = Session::with_fs(host.fs.clone(), scenario);
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(
        ran.err.contains(&format!(
            "warning: candidate solo limit {point} for core {:02} is at or deeper",
            core.get()
        )),
        "{}",
        ran.err
    );
}

#[test]
fn an_invalid_default_config_is_silent_and_the_archive_succeeds() {
    let host = Host::new();
    host.write(&host.config, "{").unwrap();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.err.contains("warning"), "{}", ran.err);
    assert!(!host.exists(session.journal()));
    assert!(host.exists(archive(session.path(), &format!("{SESSION}.jsonl"))));
}

#[test]
fn a_core_that_failed_at_zero_warns_with_the_remove_it_remedy() {
    let machine = default_machine(1).unwrap();
    machine
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: CoreId::new(0).unwrap(),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    let events = simulate(
        machine,
        togi_config::Config::parse(r#"{"start_offsets":{"0":0}}"#).unwrap(),
        None,
        Stop::cycles(1),
    )
    .unwrap();
    let host = Host::new();
    let state = support::MemoryDir::new();
    install_events_with_fs(host.fs.clone(), state.path(), &events).unwrap();
    let cfg = support::MemoryDir::new();
    let path = cfg.path().join("config.json");
    host.write(&path, r#"{"candidate_solo_limits":{"0":-5}}"#)
        .unwrap();
    let ran = reset(
        &host,
        state.path(),
        &["--all", "--config", path.to_str().unwrap()],
    );
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(
        ran.err.ends_with(
            "warning: candidate solo limit -5 for core 00 is at or deeper than its failure point 0 in the archived session; remove it\n"
        ),
        "{}",
        ran.err
    );
}

#[test_case(&["reset", "--core", "1"]; "core")]
#[test_case(&["reset", "--all"]; "all")]
fn a_closed_stdout_is_an_error(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let ran = host.run_to_closed_stdout(session.path(), args);
    assert_eq!(ran.exit, Exit::Error, "{args:?}: {}", ran.err);
    assert!(ran.err.contains("write output"), "{args:?}: {}", ran.err);
}

#[test]
fn start_offsets_are_not_solo_limits() {
    let (scenario, core, point) = failing_scenario();
    let host = Host::new();
    let cfg = support::MemoryDir::new();
    let path = cfg.path().join("config.json");
    let other = (core.get() + 1) % 16;
    host.write(
        &path,
        format!(
            r#"{{"start_offsets":{{"{other}":-5}},"candidate_solo_limits":{{"{}":{}}}}}"#,
            core.get(),
            point
        ),
    )
    .unwrap();
    let session = Session::with_fs(host.fs.clone(), scenario);
    let ran = reset(
        &host,
        session.path(),
        &["--all", "--config", path.to_str().unwrap()],
    );
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    let warnings: Vec<&str> = ran
        .err
        .lines()
        .filter(|line| line.starts_with("warning"))
        .collect();
    assert_eq!(warnings.len(), 1, "{}", ran.err);
}

#[test]
fn a_core_in_range_but_not_in_the_session_exits_2() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    let start = format!(
        "{{\"seq\":1,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"session.start\",\"msg\":\"x\",\"data\":{{\"build\":{{\"version\":\"0.0.0\",\"ruleset\":{RULESET},\"schema\":1,\"fixes\":0,\"evidence\":1}},\"session\":\"{SESSION}\",\"cores\":[{{\"core\":0,\"ccd\":0,\"cpus\":[0,1]}},{{\"core\":1,\"ccd\":0,\"cpus\":[2,3]}}]}}}}\n"
    );
    host.write(state.path().join("events.jsonl"), &start)
        .unwrap();
    let ran = reset(&host, state.path(), &["--core", "5"]);
    assert_eq!(ran.exit, Exit::Usage, "{}", ran.err);
    assert_eq!(ran.err, "togi reset: reset core 5: no such core\n");
    assert_eq!(
        host.read_to_string(state.path().join("events.jsonl"))
            .unwrap(),
        start
    );
}

#[test]
fn a_torn_tail_is_repaired_and_logged_before_the_reset() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let mut text = session.text();
    text.push_str("{\"seq\":");
    host.write(session.journal(), text).unwrap();
    let ran = reset(&host, session.path(), &["--core", "1"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    let logged: Vec<&str> = ran.err.lines().collect();
    assert_eq!(logged.len(), 2, "{}", ran.err);
    assert!(
        logged[0].starts_with("00:00:00 journal.torn "),
        "{}",
        ran.err
    );
    assert!(
        logged[1].starts_with("00:00:00 command.reset "),
        "{}",
        ran.err
    );
}

fn with_unknown_kind(session: &Session) -> String {
    let text = format!(
        "{}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"future.fact\",\"msg\":\"x\",\"data\":{{}}}}\n",
        session.text(),
        session.kinds().len() + 1
    );
    session.fs.put(session.journal(), text.as_bytes().to_vec());
    text
}

#[test]
fn an_unknown_kind_keeps_a_pending_carry_and_refuses_an_older_ruleset_too() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let text = with_unknown_kind(&session);
    host.create_dir_all(session.path().join("archive")).unwrap();
    let marker = archive(session.path(), "20250101T000000Z-carry-pending");
    host.write(&marker, "").unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(host.exists(&marker), "a refused reset dropped the carry");
    assert_eq!(
        files(&host.fs, session.path())
            .iter()
            .filter(|f| f.ends_with("-reset-all"))
            .count(),
        0
    );
    assert_eq!(session.text(), text);

    let older = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    older.rewrite_first_line(&format!("\"ruleset\":{RULESET}"), "\"ruleset\":0");
    let text = with_unknown_kind(&older);
    let ran = reset(&host, older.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert_eq!(older.text(), text);
    assert!(!host.exists(older.path().join("archive")));
}

#[test]
fn a_pending_carry_is_dropped_with_a_live_journal_and_an_empty_one() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    host.create_dir_all(session.path().join("archive")).unwrap();
    let marker = archive(session.path(), "20250101T000000Z-carry-pending");
    host.write(&marker, "").unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!host.exists(&marker));
    assert!(
        ran.out
            .starts_with(&format!("session {SESSION} archived to")),
        "{}",
        ran.out
    );

    let state = support::MemoryDir::new();
    host.create_dir_all(state.path().join("archive")).unwrap();
    host.write(state.path().join("events.jsonl"), "").unwrap();
    let marker = archive(state.path(), "20250101T000000Z-carry-pending");
    host.write(&marker, "").unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.out.starts_with("carry from session"), "{}", ran.out);
    assert!(!host.exists(&marker));
}

#[test]
fn a_torn_tail_under_all_logs_the_repair_the_reset_and_the_archive() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    let mut text = session.text();
    text.push_str("{\"seq\":");
    host.write(session.journal(), text).unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    let kinds: Vec<&str> = ran
        .err
        .lines()
        .map(|line| line.split_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["journal.torn", "command.reset", "session.archived"],
        "{}",
        ran.err
    );
}

#[test]
fn a_failed_archive_still_logs_the_events_it_recorded() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Confirm);
    host.create_dir_all(session.path().join("archive")).unwrap();
    host.write(
        archive(session.path(), &format!("{SESSION}.jsonl")),
        "taken\n",
    )
    .unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(ran.out, "");
    let first = ran.err.lines().next().unwrap();
    assert!(first.starts_with("00:00:00 command.reset "), "{}", ran.err);
    assert!(
        ran.err.lines().last().unwrap().starts_with("togi reset: "),
        "{}",
        ran.err
    );
}

#[test]
fn a_solo_limit_on_a_core_without_a_failure_point_does_not_warn() {
    let host = Host::new();
    let cfg = support::MemoryDir::new();
    let path = cfg.path().join("config.json");
    let (scenario, other) = [Scenario::Deadend, Scenario::Hunt, Scenario::Confirm]
        .into_iter()
        .find_map(|scenario| {
            let state = togi_tuner::replay(&scenario.events().unwrap()).unwrap();
            let points = state.constraints().unwrap().failure_points();
            let other = (0..16u8).find(|n| points.get(CoreId::try_from(*n).unwrap()).is_none())?;
            Some((scenario, other))
        })
        .expect("a scenario with a core that has no failure point");
    host.write(
        &path,
        format!(r#"{{"candidate_solo_limits":{{"{other}":-1}}}}"#),
    )
    .unwrap();
    let session = Session::with_fs(host.fs.clone(), scenario);
    let ran = reset(
        &host,
        session.path(),
        &["--all", "--config", path.to_str().unwrap()],
    );
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.err.contains("warning"), "{}", ran.err);
}

fn interrupted_archive() -> (Host, support::MemoryDir) {
    let host = Host::new();
    let state = support::MemoryDir::new();
    host.write(
        archive(state.path(), &format!("{SESSION}.jsonl")),
        "moved\n",
    )
    .unwrap();
    host.write(
        archive(state.path(), &format!("{SESSION}-compat-pending")),
        "",
    )
    .unwrap();
    (host, state)
}

#[test_case(0, "remove", false; "remove failure")]
#[test_case(1, "sync", true; "sync failure")]
fn interrupted_archive_removes_before_sync_and_reports_each_failed_step(
    delta: usize,
    step: &str,
    removed_after_fault: bool,
) {
    let (host, state) = interrupted_archive();
    host.fs.record_history();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    let journal = state.path().join("events.jsonl");
    let log = host.fs.log();
    let removed = log
        .iter()
        .position(|(op, path)| *op == Op::Remove && path == &journal)
        .unwrap();
    assert_eq!(log[removed + 1], (Op::SyncDir, state.path().to_path_buf()));
    assert!(!host.exists(&journal));

    let index = removed + delta;
    let (op, path) = &log[index];
    let nth = log[..=index]
        .iter()
        .filter(|(candidate, candidate_path)| {
            candidate == op && candidate_path.file_name() == path.file_name()
        })
        .count() as u64;
    let (host, state) = interrupted_archive();
    host.fs
        .inject(Fault::before(*op, nth).named(path.file_name().unwrap()));
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{step}: {}", ran.err);
    assert!(
        ran.err.starts_with(&format!("togi reset: {step} ")),
        "{}",
        ran.err
    );
    assert_eq!(!host.exists(&journal), removed_after_fault, "{step}");
    assert!(host.exists(archive(state.path(), &format!("{SESSION}-reset-all"))));
}
