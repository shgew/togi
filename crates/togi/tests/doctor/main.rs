#![allow(
    clippy::unwrap_used,
    reason = "the helpers panic on a broken fixture, as the tests do"
)]

#[path = "../support/mod.rs"]
mod support;

use std::fs;

use support::{Host, Ran, Session};
use togi_cli::Exit;
use togi_hardware::{CheckName, Diagnosis, Skip, Skipped};
use togi_hostlock::{HostLock, Policy};
use togi_machine::Check;
use togi_render::OutputFacts;

fn check(name: &str, detail: &str, ok: bool) -> Check {
    Check {
        name: name.into(),
        detail: detail.into(),
        ok,
    }
}

fn ready_checks() -> Vec<Check> {
    vec![
        check(
            "cpu",
            "family 0x1A model 0x44 AMD Ryzen 9 9950X 16-Core Processor",
            true,
        ),
        check(
            "ryzen_smu",
            "codename 23 (Granite Ridge), driver 0.7.1, SMU 98.92.0",
            true,
        ),
        check(
            "pm_table",
            "pm_table version 0x620205: decoding per-core power, voltage request, temperature and C-state lanes",
            true,
        ),
        check(
            "readback",
            "16 cores read back: 00:0 01:0 02:0 03:0 04:0 05:0 06:0 07:0 08:0 09:0 10:0 11:0 12:0 13:0 14:0 15:0",
            true,
        ),
        check(
            "slot_mapping",
            "CCD0 fuse 0x00: cores 00-07 on slots 0-7; CCD1 fuse 0x00: cores 08-15 on slots 0-7",
            true,
        ),
        check(
            "backends",
            "mprime: /nix/store/mprime/bin/mprime; y-cruncher: /nix/store/y-cruncher/bin/y-cruncher",
            true,
        ),
        check("backend_user", "togi-trial: uid 990 gid 990", true),
        check("systemd_run", "scope confined to cpu 0 created", true),
        check("watchdog", "no hardware watchdog state available", false),
    ]
}

fn ready() -> Diagnosis {
    let mut ran = ready_checks();
    ran.push(check("bios_context", "matches the session", true));
    Diagnosis {
        ran,
        skipped: Vec::new(),
        bios_context_error: None,
    }
}

fn host_with(diagnosis: Diagnosis) -> Host {
    let host = Host::new();
    *host.diagnosis.borrow_mut() = Some(Ok(diagnosis));
    host
}

fn doctor(host: &Host, state: &std::path::Path, args: &[&str]) -> Ran {
    let mut all = vec!["doctor"];
    all.extend_from_slice(args);
    host.run(state, &all)
}

fn missing_state() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    (dir, state)
}

#[test]
fn a_ready_machine_prints_the_table_and_ready() {
    let session = Session::new();
    let host = host_with(ready());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(ran.err, "");
    insta::assert_snapshot!("ready", ran.out);
}

#[test]
fn a_changed_context_is_a_warning_not_a_failure() {
    let mut diagnosis = ready();
    diagnosis.ran.pop();
    diagnosis.ran.push(check(
        "bios_context",
        "bios_version is 3.15; the session recorded 3.14; run archives this session and starts a new one",
        false,
    ));
    let session = Session::new();
    let host = host_with(diagnosis);
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(ran.err, "");
    insta::assert_snapshot!("context_changed", ran.out);
}

#[test]
fn a_failed_check_names_itself_and_exits_15() {
    let mut ran_checks = ready_checks();
    ran_checks[3] = check(
        "readback",
        "core 00: read core 00 offset: RSMU command 0xd5: status 0xfc (rejected: busy)",
        false,
    );
    ran_checks[7] = check(
        "systemd_run",
        "systemd-run: exit status 1: Failed to start transient scope unit:\tAccess denied",
        false,
    );
    let diagnosis = Diagnosis {
        ran: ran_checks,
        skipped: vec![Skipped {
            name: CheckName::BiosContext,
            reason: Skip::NeedsOtherChecks,
        }],
        bios_context_error: None,
    };
    let session = Session::new();
    let host = host_with(diagnosis);
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Preflight);
    assert_eq!(ran.err, "togi doctor: not ready: readback, systemd_run\n");
    insta::assert_snapshot!("failed", ran.out);
}

#[test]
fn without_root_it_takes_no_lock_and_says_what_it_skipped() {
    let ran_checks: Vec<Check> = ["cpu", "ryzen_smu", "backends", "backend_user", "watchdog"]
        .iter()
        .map(|name| {
            ready_checks()
                .into_iter()
                .find(|c| c.name == *name)
                .unwrap()
        })
        .collect();
    let skipped = [
        CheckName::PmTable,
        CheckName::Readback,
        CheckName::SlotMapping,
        CheckName::SystemdRun,
        CheckName::BiosContext,
    ]
    .map(|name| Skipped {
        name,
        reason: Skip::NeedsRoot,
    });
    let diagnosis = Diagnosis {
        ran: ran_checks,
        skipped: skipped.to_vec(),
        bios_context_error: None,
    };
    let session = Session::new();
    let mut host = host_with(diagnosis);
    host.root = false;
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(ran.err, "");
    assert!(!host.lock.exists(), "an unprivileged doctor takes no lock");
    assert_eq!(host.diagnoses.borrow().last().map(|d| d.1), Some(false));
    insta::assert_snapshot!("unprivileged", ran.out);
}

#[test]
fn the_failed_style_follows_the_terminal_and_the_environment() {
    let mut diagnosis = ready();
    diagnosis.ran[0].ok = false;
    for (name, facts, want) in [
        (
            "plain",
            OutputFacts::default(),
            "togi doctor: not ready: cpu\n",
        ),
        (
            "terminal",
            OutputFacts {
                is_terminal: true,
                ..OutputFacts::default()
            },
            "\x1b[1;31mtogi doctor: not ready: cpu\x1b[0m\n",
        ),
        (
            "no colour",
            OutputFacts {
                is_terminal: true,
                no_color: true,
                journald: false,
            },
            "togi doctor: not ready: cpu\n",
        ),
        (
            "journald",
            OutputFacts {
                is_terminal: false,
                no_color: false,
                journald: true,
            },
            "<3>\x1b[1;31mtogi doctor: not ready: cpu\x1b[0m\n",
        ),
    ] {
        let session = Session::new();
        let mut host = host_with(Diagnosis {
            ran: diagnosis.ran.clone(),
            skipped: Vec::new(),
            bios_context_error: None,
        });
        host.facts = facts;
        let ran = doctor(&host, session.path(), &[]);
        assert_eq!(ran.exit, Exit::Preflight, "{name}");
        assert_eq!(ran.err, want, "{name}");
        assert!(ran.out.ends_with("not ready: cpu\n"), "{name}");
        assert!(
            !ran.out.contains('\x1b'),
            "{name}: stdout is never coloured"
        );
    }
}

#[test]
fn the_recorded_context_reaches_the_checks_only_for_a_journal_run_would_continue() {
    let session = Session::new();
    let host = host_with(ready());
    assert_eq!(doctor(&host, session.path(), &[]).exit, Exit::Ok);
    let seen = host.diagnoses.borrow();
    assert_eq!(seen.as_slice(), [(Some(session.context.clone()), true)]);
    drop(seen);

    let older = Session::new();
    older.rewrite_first_line("\"ruleset\":1", "\"ruleset\":0");
    let host = host_with(ready());
    assert_eq!(doctor(&host, older.path(), &[]).exit, Exit::Ok);
    assert_eq!(host.diagnoses.borrow().as_slice(), [(None, true)]);

    let (_keep, absent) = missing_state();
    let host = host_with(ready());
    assert_eq!(doctor(&host, &absent, &[]).exit, Exit::Ok);
    assert_eq!(host.diagnoses.borrow().as_slice(), [(None, true)]);
    assert!(!absent.exists(), "doctor created the state directory");
}

#[test]
fn a_journal_run_would_refuse_exits_16_before_any_lock_or_check() {
    for (name, from, to) in [
        ("newer schema", "\"schema\":1", "\"schema\":99"),
        ("newer ruleset", "\"ruleset\":1", "\"ruleset\":99"),
        ("newer evidence", "\"evidence\":1}", "\"evidence\":99}"),
    ] {
        let session = Session::new();
        session.rewrite_first_line(from, to);
        let host = host_with(ready());
        let ran = doctor(&host, session.path(), &[]);
        assert_eq!(ran.exit, Exit::Incompatible, "{name}: {}", ran.err);
        assert_eq!(ran.out, "", "{name}");
        assert!(
            ran.err.contains("this journal was written by"),
            "{name}: {}",
            ran.err
        );
        assert_eq!(host.calls.get(), 0, "{name}");
        assert!(!host.lock.exists(), "{name}");
    }
}

#[test]
fn an_unreadable_journal_exits_1() {
    for (name, rewrite) in [("not json", true), ("malformed later event", false)] {
        let session = Session::new();
        if rewrite {
            fs::write(session.journal(), "not json\n").unwrap();
        } else {
            let mut text = session.text();
            text.push_str("{\"seq\":\n");
            let at = text.find("\"kind\":\"config.loaded\"").unwrap();
            text.replace_range(at..at + 4, "\"kidn\"");
            fs::write(session.journal(), text).unwrap();
        }
        let host = host_with(ready());
        let ran = doctor(&host, session.path(), &[]);
        assert_eq!(ran.exit, Exit::Error, "{name}: {}", ran.err);
        assert_eq!(ran.out, "", "{name}");
        assert_eq!(host.calls.get(), 0, "{name}");
    }
}

#[test]
fn a_configuration_error_exits_2() {
    let cfg = tempfile::tempdir().unwrap();
    let bad = cfg.path().join("bad.json");
    fs::write(&bad, "{").unwrap();
    let missing = cfg.path().join("missing.json");
    for path in [&bad, &missing] {
        let session = Session::new();
        let host = host_with(ready());
        let ran = doctor(&host, session.path(), &["--config", path.to_str().unwrap()]);
        assert_eq!(ran.exit, Exit::Usage, "{}", ran.err);
        assert!(ran.err.contains(path.to_str().unwrap()), "{}", ran.err);
        assert_eq!(host.calls.get(), 0);
    }
}

#[test]
fn config_is_accepted_before_and_after_the_command() {
    let cfg = tempfile::tempdir().unwrap();
    let missing = cfg.path().join("missing.json");
    let missing = missing.to_str().unwrap();
    for args in [
        &["doctor", "--config", missing][..],
        &["--config", missing, "doctor"],
    ] {
        let session = Session::new();
        let host = host_with(ready());
        let ran = host.run(session.path(), args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}");
        assert!(ran.err.contains("missing.json"), "{args:?}: {}", ran.err);
    }
}

#[test]
fn a_busy_host_lock_exits_3_only_for_root() {
    let session = Session::new();
    let host = host_with(ready());
    let _held = HostLock::acquire(&host.lock, Policy::Private).unwrap();
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    assert!(ran.err.contains(host.lock.to_str().unwrap()), "{}", ran.err);
    assert_eq!(host.calls.get(), 0);

    let mut host = host_with(ready());
    host.root = false;
    host.lock = host.lock.clone();
    let _held = HostLock::acquire(&host.lock, Policy::Private).unwrap();
    assert_eq!(doctor(&host, session.path(), &[]).exit, Exit::Ok);
}

#[test]
fn a_host_lock_that_cannot_be_opened_exits_1() {
    let session = Session::new();
    let mut host = host_with(ready());
    host.lock = host
        .lock
        .parent()
        .unwrap()
        .join("missing")
        .join("togi.lock");
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn a_failed_diagnosis_exits_1() {
    let session = Session::new();
    let host = Host::new();
    *host.diagnosis.borrow_mut() = Some(Err(togi_hardware::Error::NeedsRoot {
        operation: "open the SMU",
        uid: 1000,
    }));
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(ran.out, "");
    assert!(
        ran.err
            .starts_with("togi doctor: open the SMU: running as uid 1000"),
        "{}",
        ran.err
    );
}

#[test]
fn a_bios_context_that_cannot_be_read_follows_the_table_with_exit_1() {
    let diagnosis = Diagnosis {
        ran: ready_checks(),
        skipped: Vec::new(),
        bios_context_error: Some(togi_smu::Error::Unsupported),
    };
    let session = Session::new();
    let host = host_with(diagnosis);
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(ran.out.starts_with("CHECK"), "{}", ran.out);
    assert!(!ran.out.contains("ready"), "{}", ran.out);
    assert_eq!(
        ran.err,
        "togi doctor: read BIOS context: the SMU driver needs Linux\n"
    );
}

#[test]
fn another_platform_stops_before_the_journal_the_lock_and_every_check() {
    let session = Session::new();
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let (_keep, absent) = missing_state();
    for (name, state) in [
        ("a journal run would refuse", session.path()),
        ("no state directory", &absent),
    ] {
        let mut host = host_with(ready());
        host.linux = false;
        let held = HostLock::acquire(&host.lock, Policy::Private).unwrap();
        let cfg = state.join("no-such-config.json");
        let ran = doctor(&host, state, &["--config", cfg.to_str().unwrap()]);
        drop(held);
        assert_eq!(ran.exit, Exit::Error, "{name}: {}", ran.err);
        assert_eq!(ran.out, "", "{name}");
        assert_eq!(
            ran.err, "togi doctor: hardware runs need Linux: unsupported operation\n",
            "{name}"
        );
        assert_eq!(host.calls.get(), 0, "{name}");
    }
    assert!(!absent.exists(), "doctor created the state directory");

    let mut host = host_with(ready());
    host.linux = false;
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(!host.lock.exists(), "the host lock file was created");
}

#[test]
fn a_closed_stdout_is_an_error() {
    let session = Session::new();
    let host = host_with(ready());
    let ran = host.run_to_closed_stdout(session.path(), &["doctor"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert!(ran.err.contains("write output"), "{}", ran.err);
}

#[test]
fn an_unknown_event_kind_in_a_current_journal_exits_16_naming_it() {
    let session = Session::new();
    let mut text = session.text();
    text = format!(
        "{text}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"future.fact\",\"msg\":\"x\",\"data\":{{}}}}\n",
        session.kinds().len() + 1
    );
    fs::write(session.journal(), text).unwrap();
    let host = host_with(ready());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert!(
        ran.err.contains("unknown event kind \"future.fact\""),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains("this journal was written by togi"),
        "{}",
        ran.err
    );
    assert_eq!(host.calls.get(), 0);
}
