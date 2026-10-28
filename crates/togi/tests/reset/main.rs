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
use togi_tuner::RULESET;
use togi_watchtest::Scenario;

const SESSION: &str = "20260101T000000Z";

fn reset(host: &Host, state: &Path, args: &[&str]) -> Ran {
    let mut all = vec!["reset"];
    all.extend_from_slice(args);
    host.run(state, &all)
}

fn archive(state: &Path, name: &str) -> std::path::PathBuf {
    state.join("archive").join(name)
}

#[test_case(&[]; "no target")]
#[test_case(&["--core", "1"]; "the removed core target")]
#[test_case(&["--core", "1", "--all"]; "the removed core target with all")]
#[test_case(&["--all", "--all"]; "all twice")]
#[test_case(&["--all", "extra"]; "a stray argument")]
fn reset_takes_exactly_all(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
    session.rewrite_first_line(&format!("\"ruleset\":{RULESET}"), "\"ruleset\":99");
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.out.contains("without appending"), "{}", ran.out);
    let archived = host
        .read_to_string(archive(session.path(), &format!("{SESSION}.jsonl")))
        .unwrap();
    assert!(archived.contains("\"kind\":\"command.reset\""));
}

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

#[test_case(&["--all"]; "all")]
fn a_busy_host_lock_exits_3_naming_it_before_anything_changes(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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

#[test_case(&["--all"]; "all")]
fn a_held_state_directory_lock_exits_3_and_changes_nothing(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
fn a_pending_import_is_dropped_without_a_journal() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    host.create_dir_all(state.path().join("archive")).unwrap();
    host.write(
        archive(state.path(), &format!("{SESSION}-import-pending")),
        "20260101T000100Z\n",
    )
    .unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!(
            "import from session {SESSION} dropped; the next togi run starts a new session with nothing imported\n"
        )
    );
    assert!(!host.exists(archive(state.path(), &format!("{SESSION}-import-pending"))));
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

#[test]
fn error_text_escapes_control_characters() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    let nasty = state.path().join("a\x1b[31mb");
    let ran = reset(&host, &nasty, &["--all"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(!ran.err.contains('\x1b'), "{:?}", ran.err);
}

#[test]
fn a_held_writer_lock_leaves_a_pending_import_alone() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    host.create_dir_all(state.path().join("archive")).unwrap();
    let marker = archive(state.path(), &format!("{SESSION}-import-pending"));
    host.write(&marker, "20260101T000100Z\n").unwrap();
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
    host.write(
        archive(&state, &format!("{SESSION}-import-pending")),
        "20260101T000100Z\n",
    )
    .unwrap();
    let ran = reset(&host, &state, &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.out.starts_with("import from session"), "{}", ran.out);
}

#[test]
fn an_incompatible_archive_refuses_a_collision_and_resumes_a_pending_move() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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

#[test_case(&["--all"]; "all")]
fn a_current_journal_with_an_unknown_kind_exits_16_and_is_left_alone(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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

#[test_case(&["--all"]; "all")]
fn a_current_journal_with_an_unknown_field_exits_16_and_is_left_alone(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
fn real_fs_a_host_lock_that_cannot_be_opened_exits_1() {
    let mut host = Host::real();
    host.lock = host
        .lock
        .parent()
        .unwrap()
        .join("missing")
        .join("togi.lock");
    let ran = reset(&host, &host.lock.with_file_name("state"), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
}

#[test]
fn infrastructure_failures_exit_1() {
    let mut host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
    host.boot_fails = true;
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);

    let host = session.host();
    let file = session.path().join("events.jsonl");
    let ran = reset(&host, &file, &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
}

#[test_case(&["reset", "--all"]; "all")]
fn a_closed_stdout_is_an_error(args: &[&str]) {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
    let ran = host.run_to_closed_stdout(session.path(), args);
    assert_eq!(ran.exit, Exit::Error, "{args:?}: {}", ran.err);
    assert!(ran.err.contains("write output"), "{args:?}: {}", ran.err);
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
fn an_unknown_kind_keeps_a_pending_import_and_refuses_an_older_ruleset_too() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
    let text = with_unknown_kind(&session);
    host.create_dir_all(session.path().join("archive")).unwrap();
    let marker = archive(session.path(), "20250101T000000Z-import-pending");
    host.write(&marker, "20250101T000100Z\n").unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(host.exists(&marker), "a refused reset dropped the import");
    assert_eq!(
        files(&host.fs, session.path())
            .iter()
            .filter(|f| f.ends_with("-reset-all"))
            .count(),
        0
    );
    assert_eq!(session.text(), text);

    let older = Session::with_fs(host.fs.clone(), Scenario::Controls);
    older.rewrite_first_line(&format!("\"ruleset\":{RULESET}"), "\"ruleset\":0");
    let text = with_unknown_kind(&older);
    let ran = reset(&host, older.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert_eq!(older.text(), text);
    assert!(!host.exists(older.path().join("archive")));
}

#[test]
fn a_pending_import_is_dropped_with_a_live_journal_and_an_empty_one() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
    host.create_dir_all(session.path().join("archive")).unwrap();
    let marker = archive(session.path(), "20250101T000000Z-import-pending");
    host.write(&marker, "20250101T000100Z\n").unwrap();
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
    let marker = archive(state.path(), "20250101T000000Z-import-pending");
    host.write(&marker, "20250101T000100Z\n").unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.out.starts_with("import from session"), "{}", ran.out);
    assert!(!host.exists(&marker));
}

#[test]
fn a_torn_tail_under_all_logs_the_repair_the_reset_and_the_archive() {
    let host = Host::new();
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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
    let session = Session::with_fs(host.fs.clone(), Scenario::Controls);
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

#[test_case(0, "remove marker", false; "remove failure")]
#[test_case(1, "sync directory", true; "sync failure")]
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
    let marker = archive(state.path(), &format!("{SESSION}-compat-pending"));
    let archived = archive(state.path(), &format!("{SESSION}.jsonl"));
    let archive_dir = state.path().join("archive");
    let log = host.fs.log();
    let removed = log
        .iter()
        .position(|(op, path)| *op == Op::Remove && path == &marker)
        .unwrap();
    assert_eq!(log[removed + 1], (Op::SyncDir, archive_dir));
    assert!(!host.exists(&journal));
    assert!(
        !log.iter()
            .any(|(op, path)| *op == Op::Remove && path == &journal)
    );
    assert_eq!(host.read_to_string(&archived).unwrap(), "moved\n");

    let index = removed + delta;
    let (op, path) = &log[index];
    let nth = log[..=index]
        .iter()
        .filter(|(candidate, candidate_path)| {
            candidate == op && candidate_path.file_name() == path.file_name()
        })
        .count() as u64;
    let (host, state) = interrupted_archive();
    let journal = state.path().join("events.jsonl");
    host.fs
        .inject(Fault::before(*op, nth).named(path.file_name().unwrap()));
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{step}: {}", ran.err);
    assert!(
        ran.err.starts_with(&format!("togi reset: {step} ")),
        "{}",
        ran.err
    );
    let marker = archive(state.path(), &format!("{SESSION}-compat-pending"));
    assert_eq!(!host.exists(&marker), removed_after_fault, "{step}");
    assert!(!host.exists(&journal));
    assert_eq!(
        host.read_to_string(archive(state.path(), &format!("{SESSION}.jsonl")))
            .unwrap(),
        "moved\n"
    );
    assert!(host.exists(archive(state.path(), &format!("{SESSION}-reset-all"))));
}
