#![allow(
    clippy::unwrap_used,
    reason = "the helpers panic on a broken fixture, as the tests do"
)]

#[path = "../support/mod.rs"]
mod support;

use std::fs;
use std::path::Path;

use support::{Host, Ran, Session, files};
use togi_cli::Exit;
use togi_hostlock::{HostLock, Policy};
use togi_journal::StateDir;
use togi_machine::{CoreId, Signal};
use togi_sim::model::Script;
use togi_watchtest::{Scenario, Stop, default_machine, install_events, simulate};

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
    let session = Session::new();
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

#[test]
fn reset_core_of_a_core_the_session_lacks_exits_2_and_changes_nothing() {
    let host = Host::new();
    let session = Session::new();
    let before = session.text();
    for core in ["16", "99"] {
        let ran = reset(&host, session.path(), &["--core", core]);
        assert_eq!(ran.exit, Exit::Usage, "{core}: {}", ran.err);
        assert_eq!(ran.out, "");
        assert_eq!(
            ran.err,
            format!("togi reset: reset core {core}: no such core\n")
        );
        assert_eq!(session.text(), before, "{core}");
    }
}

#[test]
fn exactly_one_of_core_and_all_and_a_core_number() {
    for args in [
        &[][..],
        &["--core", "1", "--all"],
        &["--core", "x"],
        &["--core", "-1"],
        &["--core", "300"],
    ] {
        let host = Host::new();
        let session = Session::new();
        let before = files(session.path());
        let ran = reset(&host, session.path(), args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "", "{args:?}");
        assert_eq!(files(session.path()), before, "{args:?}");
        assert!(!host.lock.exists(), "{args:?}: the host lock was taken");
    }
}

#[test]
fn no_journal_or_no_session_exits_1_without_creating_state() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        let ran = reset(&host, state.path(), args);
        assert_eq!(ran.exit, Exit::Error, "{args:?}");
        assert!(ran.err.contains("no journal at"), "{}", ran.err);
        assert_eq!(files(state.path()), Vec::<String>::new(), "{args:?}");

        for content in ["", "{\"seq\":"] {
            fs::write(state.path().join("events.jsonl"), content).unwrap();
            let before = files(state.path());
            let ran = reset(&host, state.path(), args);
            assert_eq!(ran.exit, Exit::Error, "{args:?}");
            assert_eq!(
                ran.err,
                format!("togi reset: no session in {}\n", state.path().display())
            );
            assert_eq!(files(state.path()), before, "{args:?}: {content:?}");
            assert_eq!(
                fs::read_to_string(state.path().join("events.jsonl")).unwrap(),
                content,
                "{args:?}"
            );
        }
    }
}

#[test]
fn reset_all_archives_the_session_and_marks_the_boundary() {
    let host = Host::new();
    let session = Session::new();
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
    assert!(!session.journal().exists());
    let archived =
        fs::read_to_string(archive(session.path(), &format!("{SESSION}.jsonl"))).unwrap();
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
    assert!(archive(session.path(), &format!("{SESSION}-reset-all")).exists());
}

#[test]
fn reset_all_of_an_unrecognized_schema_archives_it_unread() {
    let host = Host::new();
    let session = Session::new();
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
        fs::read_to_string(archive(session.path(), &format!("{SESSION}.jsonl"))).unwrap(),
        original
    );
    assert!(archive(session.path(), &format!("{SESSION}-reset-all")).exists());
    assert!(!session.journal().exists());
}

#[test]
fn reset_all_of_another_ruleset_archives_it_normally() {
    let host = Host::new();
    let session = Session::new();
    session.rewrite_first_line("\"ruleset\":1", "\"ruleset\":99");
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.out.contains("without appending"), "{}", ran.out);
    let archived =
        fs::read_to_string(archive(session.path(), &format!("{SESSION}.jsonl"))).unwrap();
    assert!(archived.contains("\"kind\":\"command.reset\""));
}

#[test]
fn reset_core_refuses_every_stamp_mismatch_with_exit_16_and_appends_nothing() {
    let archives = "togi run archives it and starts a new session that carries its candidate solo limits and failure points; togi reset --all archives it and starts over.";
    let install = "Install togi 0.0.0 to continue this session, or run togi reset --all to archive it and start over.";
    for (name, rewrites, written, uses, advice) in [
        (
            "newer schema",
            &[("\"schema\":1", "\"schema\":99")][..],
            "(schema 99, ruleset 1)",
            "schema 1",
            install,
        ),
        (
            "older schema",
            &[("\"schema\":1", "\"schema\":0")],
            "(schema 0, ruleset 1)",
            "schema 1",
            archives,
        ),
        (
            "newer ruleset",
            &[("\"ruleset\":1", "\"ruleset\":99")],
            "(schema 1, ruleset 99)",
            "ruleset 1",
            install,
        ),
        (
            "newer evidence",
            &[("\"evidence\":1}", "\"evidence\":99}")],
            "(schema 1, ruleset 1, evidence epoch 99)",
            "evidence epoch 1",
            install,
        ),
        (
            "older ruleset",
            &[("\"ruleset\":1", "\"ruleset\":0")],
            "(schema 1, ruleset 0)",
            "ruleset 1",
            archives,
        ),
        (
            "older ruleset with a newer evidence epoch",
            &[
                ("\"ruleset\":1", "\"ruleset\":0"),
                ("\"evidence\":1}", "\"evidence\":99}"),
            ],
            "(schema 1, ruleset 0)",
            "ruleset 1",
            install,
        ),
    ] {
        let host = Host::new();
        let session = Session::new();
        for (from, to) in rewrites {
            session.rewrite_first_line(from, to);
        }
        session.rewrite_first_line(
            "\"version\":\"0.0.0\"",
            "\"version\":\"0.0.0\",\"rev\":\"abc1234\"",
        );
        let before = session.text();
        let ran = reset(&host, session.path(), &["--core", "0"]);
        assert_eq!(ran.exit, Exit::Incompatible, "{name}: {}", ran.err);
        assert_eq!(ran.out, "", "{name}");
        assert!(
            ran.err.starts_with(&format!(
                "togi reset: this journal was written by togi 0.0.0+abc1234 {written}; this build, togi "
            )),
            "{name}: {}",
            ran.err
        );
        assert!(
            ran.err.ends_with(&format!(", uses {uses}. {advice}\n")),
            "{name}: {}",
            ran.err
        );
        assert_eq!(session.text(), before, "{name}");
    }
}

#[test]
fn an_unreadable_journal_exits_1() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        fs::write(state.path().join("events.jsonl"), "not json\n").unwrap();
        let ran = reset(&host, state.path(), args);
        assert_eq!(ran.exit, Exit::Error, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "");
    }
}

#[test]
fn a_busy_host_lock_exits_3_naming_it_before_anything_changes() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let session = Session::new();
        let before = files(session.path());
        let text = session.text();
        let _held = HostLock::acquire(&host.lock, Policy::Private).unwrap();
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
        assert_eq!(files(session.path()), before);
        assert_eq!(session.text(), text);
    }
}

#[test]
fn a_held_state_directory_lock_exits_3_and_changes_nothing() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let session = Session::new();
        let text = session.text();
        let held = StateDir::lock(session.path()).unwrap();
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
}

#[test]
fn a_pending_carry_is_dropped_without_a_journal() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(state.path().join("archive")).unwrap();
    fs::write(
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
    assert!(!archive(state.path(), &format!("{SESSION}-carry-pending")).exists());
    assert!(archive(state.path(), &format!("{SESSION}-reset-all")).exists());
}

#[test]
fn reset_all_completes_an_interrupted_incompatible_archive() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(state.path().join("archive")).unwrap();
    fs::write(
        archive(state.path(), &format!("{SESSION}.jsonl")),
        "moved\n",
    )
    .unwrap();
    fs::write(
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
    assert!(!archive(state.path(), &format!("{SESSION}-compat-pending")).exists());
    assert!(archive(state.path(), &format!("{SESSION}-reset-all")).exists());
    assert!(
        !state.path().join("events.jsonl").exists(),
        "reset recreated the journal"
    );
    assert_eq!(
        fs::read_to_string(archive(state.path(), &format!("{SESSION}.jsonl"))).unwrap(),
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

#[test]
fn reset_all_warns_for_each_solo_limit_at_or_past_its_failure_point() {
    let (scenario, core, point) = failing_scenario();
    let core = core.get();
    assert!(point < 0, "this case pins the remedy that offers a limit");
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let write = |limit: i64| {
        let path = cfg.path().join(format!("config{limit}.json"));
        fs::write(
            &path,
            format!(r#"{{"candidate_solo_limits":{{"{core}":{limit}}}}}"#),
        )
        .unwrap();
        path
    };
    let cases = [(point, true), (point - 1, true), (point + 1, false)];
    for (limit, warns) in cases {
        if !(-50..=0).contains(&limit) {
            continue;
        }
        let session = Session::of(scenario);
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
}

#[test]
fn an_invalid_or_missing_config_never_stops_the_archive() {
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let bad = cfg.path().join("bad.json");
    fs::write(&bad, "{").unwrap();
    let missing = cfg.path().join("missing.json");
    for path in [&bad, &missing] {
        let session = Session::new();
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
        assert!(!session.journal().exists());
    }
}

#[test]
fn config_is_accepted_before_and_after_the_command() {
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let missing = cfg.path().join("missing.json");
    let missing = missing.to_str().unwrap();
    for args in [
        &["reset", "--all", "--config", missing][..],
        &["--config", missing, "reset", "--all"],
    ] {
        let session = Session::new();
        let ran = host.run(session.path(), args);
        assert_eq!(ran.exit, Exit::Ok, "{args:?}: {}", ran.err);
        assert!(
            ran.err.contains("cannot check candidate solo limits"),
            "{args:?}: {}",
            ran.err
        );
    }
}

#[test]
fn error_text_escapes_control_characters() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    let nasty = state.path().join("a\x1b[31mb");
    let ran = reset(&host, &nasty, &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(!ran.err.contains('\x1b'), "{:?}", ran.err);
}

#[test]
fn a_held_writer_lock_leaves_a_pending_carry_alone() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(state.path().join("archive")).unwrap();
    let marker = archive(state.path(), &format!("{SESSION}-carry-pending"));
    fs::write(&marker, "").unwrap();
    let held = StateDir::lock(state.path()).unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    drop(held);
    assert!(marker.exists());
    assert!(!archive(state.path(), &format!("{SESSION}-reset-all")).exists());
}

#[test]
fn pending_markers_are_found_in_a_state_directory_named_like_a_pattern() {
    let host = Host::new();
    let parent = tempfile::tempdir().unwrap();
    let state = parent.path().join("state[1]*?{a,b}");
    fs::create_dir_all(state.join("archive")).unwrap();
    fs::write(archive(&state, &format!("{SESSION}-carry-pending")), "").unwrap();
    let ran = reset(&host, &state, &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.out.starts_with("carry from session"), "{}", ran.out);
}

#[test]
fn an_incompatible_archive_refuses_a_collision_and_resumes_a_pending_move() {
    let host = Host::new();
    let session = Session::new();
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let original = session.text();
    fs::create_dir(session.path().join("archive")).unwrap();
    let target = archive(session.path(), &format!("{SESSION}.jsonl"));
    fs::write(&target, "taken\n").unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(session.text(), original);
    assert_eq!(fs::read_to_string(&target).unwrap(), "taken\n");

    fs::remove_file(&target).unwrap();
    fs::write(
        archive(session.path(), &format!("{SESSION}-compat-pending")),
        "",
    )
    .unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
    assert!(!session.journal().exists());
    assert!(!archive(session.path(), &format!("{SESSION}-compat-pending")).exists());
}

#[test]
fn a_journal_that_already_records_its_archive_is_only_moved() {
    let host = Host::new();
    let session = Session::new();
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let mut text = session.text();
    text = format!(
        "{text}{{\"seq\":9999,\"kind\":\"session.archived\",\"data\":{{\"session\":\"{SESSION}\"}}}}\n"
    );
    fs::write(session.journal(), &text).unwrap();
    fs::create_dir(session.path().join("archive")).unwrap();
    fs::write(
        archive(session.path(), &format!("{SESSION}-trials")),
        "kept\n",
    )
    .unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        fs::read_to_string(archive(session.path(), &format!("{SESSION}.jsonl"))).unwrap(),
        text
    );
    assert_eq!(
        fs::read_to_string(archive(session.path(), &format!("{SESSION}-trials"))).unwrap(),
        "kept\n"
    );
    assert!(archive(session.path(), &format!("{SESSION}-reset-all")).exists());
}

#[test]
fn a_current_journal_with_an_unknown_kind_exits_16_and_is_left_alone() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let session = Session::new();
        let mut text = session.text();
        text = format!(
            "{text}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"future.fact\",\"msg\":\"x\",\"data\":{{}}}}\n",
            session.kinds().len() + 1
        );
        fs::write(session.journal(), &text).unwrap();
        let ran = reset(&host, session.path(), args);
        assert_eq!(ran.exit, Exit::Incompatible, "{args:?}: {}", ran.err);
        assert!(
            ran.err.starts_with(
                "togi reset: unknown kind \"future.fact\" in journal written by togi 0.0.0 (schema 1, ruleset 1); this build, togi "
            ),
            "{args:?}: {}",
            ran.err
        );
        assert!(
            ran.err.ends_with(
                " (schema 1, ruleset 1), cannot safely modify it; install the build that wrote it\n"
            ),
            "{args:?}: {}",
            ran.err
        );
        assert!(!ran.err.contains("reset --all"), "{args:?}: {}", ran.err);
        assert_eq!(session.text(), text, "{args:?}");
        assert!(!session.path().join("archive").exists(), "{args:?}");
    }
}

#[test]
fn solo_limits_are_checked_across_rulesets_and_skipped_across_schemas() {
    let (scenario, core, point) = failing_scenario();
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let path = cfg.path().join("config.json");
    fs::write(
        &path,
        format!(
            r#"{{"candidate_solo_limits":{{"{}":{}}}}}"#,
            core.get(),
            point
        ),
    )
    .unwrap();
    let path = path.to_str().unwrap();
    let ruleset = Session::of(scenario);
    ruleset.rewrite_first_line("\"ruleset\":1", "\"ruleset\":99");
    let ran = reset(&host, ruleset.path(), &["--all", "--config", path]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.err.contains("candidate solo limit"), "{}", ran.err);

    let schema = Session::of(scenario);
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
    let session = Session::new();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.err.contains("warning"), "{}", ran.err);
}

#[test]
fn infrastructure_failures_exit_1() {
    let session = Session::new();
    let mut host = Host::new();
    host.lock = host
        .lock
        .parent()
        .unwrap()
        .join("missing")
        .join("togi.lock");
    let ran = reset(&host, session.path(), &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);

    let mut host = Host::new();
    host.boot_fails = true;
    let ran = reset(&host, session.path(), &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);

    let host = Host::new();
    let file = session.path().join("events.jsonl");
    let ran = reset(&host, &file, &["--all"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
}

#[test]
fn an_invalid_default_config_is_silent_and_the_archive_succeeds() {
    let host = Host::new();
    fs::write(&host.config, "{").unwrap();
    let session = Session::new();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.err.contains("warning"), "{}", ran.err);
    assert!(!session.journal().exists());
    assert!(archive(session.path(), &format!("{SESSION}.jsonl")).exists());
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
    let state = tempfile::tempdir().unwrap();
    install_events(state.path(), &events).unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let path = cfg.path().join("config.json");
    fs::write(&path, r#"{"candidate_solo_limits":{"0":-5}}"#).unwrap();
    let host = Host::new();
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

#[test]
fn a_closed_stdout_is_an_error() {
    for args in [&["reset", "--core", "1"][..], &["reset", "--all"]] {
        let host = Host::new();
        let session = Session::new();
        let ran = host.run_to_closed_stdout(session.path(), args);
        assert_eq!(ran.exit, Exit::Error, "{args:?}: {}", ran.err);
        assert!(ran.err.contains("write output"), "{args:?}: {}", ran.err);
    }
}

#[test]
fn start_offsets_are_not_solo_limits() {
    let (scenario, core, point) = failing_scenario();
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let path = cfg.path().join("config.json");
    let other = (core.get() + 1) % 16;
    fs::write(
        &path,
        format!(
            r#"{{"start_offsets":{{"{other}":-5}},"candidate_solo_limits":{{"{}":{}}}}}"#,
            core.get(),
            point
        ),
    )
    .unwrap();
    let session = Session::of(scenario);
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
    let state = tempfile::tempdir().unwrap();
    let start = format!(
        "{{\"seq\":1,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"session.start\",\"msg\":\"x\",\"data\":{{\"build\":{{\"version\":\"0.0.0\",\"ruleset\":1,\"schema\":1,\"fixes\":0,\"evidence\":1}},\"session\":\"{SESSION}\",\"cores\":[{{\"core\":0,\"ccd\":0,\"cpus\":[0,1]}},{{\"core\":1,\"ccd\":0,\"cpus\":[2,3]}}]}}}}\n"
    );
    fs::write(state.path().join("events.jsonl"), &start).unwrap();
    let ran = reset(&host, state.path(), &["--core", "5"]);
    assert_eq!(ran.exit, Exit::Usage, "{}", ran.err);
    assert_eq!(ran.err, "togi reset: reset core 5: no such core\n");
    assert_eq!(
        fs::read_to_string(state.path().join("events.jsonl")).unwrap(),
        start
    );
}

#[test]
fn a_torn_tail_is_repaired_and_logged_before_the_reset() {
    let host = Host::new();
    let session = Session::new();
    let mut text = session.text();
    text.push_str("{\"seq\":");
    fs::write(session.journal(), text).unwrap();
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
    fs::write(session.journal(), &text).unwrap();
    text
}

#[test]
fn an_unknown_kind_keeps_a_pending_carry_and_refuses_an_older_ruleset_too() {
    let host = Host::new();
    let session = Session::new();
    let text = with_unknown_kind(&session);
    fs::create_dir(session.path().join("archive")).unwrap();
    let marker = archive(session.path(), "20250101T000000Z-carry-pending");
    fs::write(&marker, "").unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(marker.exists(), "a refused reset dropped the carry");
    assert_eq!(
        files(session.path())
            .iter()
            .filter(|f| f.ends_with("-reset-all"))
            .count(),
        0
    );
    assert_eq!(session.text(), text);

    let older = Session::new();
    older.rewrite_first_line("\"ruleset\":1", "\"ruleset\":0");
    let text = with_unknown_kind(&older);
    let ran = reset(&host, older.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert_eq!(older.text(), text);
    assert!(!older.path().join("archive").exists());
}

#[test]
fn a_pending_carry_is_dropped_with_a_live_journal_and_an_empty_one() {
    let host = Host::new();
    let session = Session::new();
    fs::create_dir(session.path().join("archive")).unwrap();
    let marker = archive(session.path(), "20250101T000000Z-carry-pending");
    fs::write(&marker, "").unwrap();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!marker.exists());
    assert!(
        ran.out
            .starts_with(&format!("session {SESSION} archived to")),
        "{}",
        ran.out
    );

    let state = tempfile::tempdir().unwrap();
    fs::create_dir(state.path().join("archive")).unwrap();
    fs::write(state.path().join("events.jsonl"), "").unwrap();
    let marker = archive(state.path(), "20250101T000000Z-carry-pending");
    fs::write(&marker, "").unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(ran.out.starts_with("carry from session"), "{}", ran.out);
    assert!(!marker.exists());
}

#[test]
fn a_torn_tail_under_all_logs_the_repair_the_reset_and_the_archive() {
    let host = Host::new();
    let session = Session::new();
    let mut text = session.text();
    text.push_str("{\"seq\":");
    fs::write(session.journal(), text).unwrap();
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
    let session = Session::new();
    fs::create_dir(session.path().join("archive")).unwrap();
    fs::write(
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
    let cfg = tempfile::tempdir().unwrap();
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
    fs::write(
        &path,
        format!(r#"{{"candidate_solo_limits":{{"{other}":-1}}}}"#),
    )
    .unwrap();
    let session = Session::of(scenario);
    let ran = reset(
        &host,
        session.path(),
        &["--all", "--config", path.to_str().unwrap()],
    );
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.err.contains("warning"), "{}", ran.err);
}
