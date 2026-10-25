#![allow(
    clippy::unwrap_used,
    reason = "the helpers panic on a broken fixture, as the tests do"
)]

#[path = "../support/mod.rs"]
mod support;

use std::sync::Arc;
use support::{Host, Ran, Session, files};
use togi_cli::Exit;
use togi_hardware::{CheckName, Diagnosis, Skip, Skipped};
use togi_journal::MemFs;
use togi_machine::Check;
use togi_render::OutputFacts;
use togi_tuner::RULESET;

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

fn host_with(diagnosis: Diagnosis, fs: Arc<MemFs>) -> Host {
    let host = Host::with_fs(fs);
    *host.diagnosis.borrow_mut() = Some(Ok(diagnosis));
    host
}

fn doctor(host: &Host, state: &std::path::Path, args: &[&str]) -> Ran {
    let mut all = vec!["doctor"];
    all.extend_from_slice(args);
    host.run(state, &all)
}

fn missing_state() -> (support::MemoryDir, std::path::PathBuf) {
    let dir = support::MemoryDir::new();
    let state = dir.path().join("state");
    (dir, state)
}

#[test]
fn a_ready_machine_prints_the_table_and_ready() {
    let session = Session::new();
    let host = host_with(ready(), session.fs.clone());
    let before = (files(&session.fs, session.path()), session.text());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        host.lock_busy_in_diagnose.get(),
        Some(true),
        "the host lock was not held during the checks"
    );
    assert_eq!(ran.err, "");
    assert_eq!((files(&session.fs, session.path()), session.text()), before);
    assert_snapshot(include_str!("snapshots/doctor__ready.snap"), &ran.out);
}

#[test]
fn a_four_ccd_machine_is_not_ready() {
    let session = Session::new();
    let mut host = host_with(ready(), session.fs.clone());
    host.topology = (0..4)
        .map(|n| togi_machine::CoreInfo {
            core: togi_machine::CoreId::new(n).unwrap(),
            ccd: togi_machine::CcdId::new(n).unwrap(),
            cpus: vec![togi_machine::CpuId::from(u16::try_from(n).unwrap())],
        })
        .collect();
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Preflight, "{}", ran.out);
    assert!(ran.out.contains("not supported"), "{}", ran.out);
    assert!(ran.out.ends_with("not ready: topology\n"), "{}", ran.out);
}

#[test]
fn a_changed_context_is_a_warning_not_a_failure() {
    let mut diagnosis = ready();
    diagnosis.ran.pop();
    diagnosis.ran.push(check(
        "bios_context",
        "bios_version is 3.15; the session recorded 3.14; run records the new context and the session continues, re-qualifying under it",
        false,
    ));
    let session = Session::new();
    let host = host_with(diagnosis, session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(ran.err, "");
    assert_snapshot(
        include_str!("snapshots/doctor__context_changed.snap"),
        &ran.out,
    );
}

#[test]
fn an_unavailable_backend_is_a_warning_not_a_failure() {
    let mut diagnosis = ready();
    let backends = diagnosis
        .ran
        .iter_mut()
        .find(|check| check.name == "backends")
        .unwrap();
    *backends = check(
        "backends",
        "backends.mprime not configured; ycruncher: /nix/store/y-cruncher/bin/y-cruncher",
        false,
    );
    let session = Session::new();
    let host = host_with(diagnosis, session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(ran.err, "");
    assert!(
        ran.out
            .lines()
            .any(|line| line.starts_with("backends      warn    backends.mprime not configured")),
        "{}",
        ran.out
    );
    assert!(ran.out.ends_with("\nready\n"), "{}", ran.out);
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
    let host = host_with(diagnosis, session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Preflight);
    assert_eq!(ran.err, "togi doctor: not ready: readback, systemd_run\n");
    assert_snapshot(include_str!("snapshots/doctor__failed.snap"), &ran.out);
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
    let mut host = host_with(diagnosis, session.fs.clone());
    host.root = false;
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(ran.err, "");
    assert_eq!(
        host.lock_acquisitions.get(),
        0,
        "an unprivileged doctor takes no lock"
    );
    assert_eq!(host.diagnoses.borrow().last().map(|d| d.1), Some(false));
    assert_snapshot(
        include_str!("snapshots/doctor__unprivileged.snap"),
        &ran.out,
    );
}

#[test_case::test_case(OutputFacts::default(), "togi doctor: not ready: cpu\n"; "plain")]
#[test_case::test_case(OutputFacts { is_terminal: true, ..OutputFacts::default() }, "\x1b[1;31mtogi doctor: not ready: cpu\x1b[0m\n"; "terminal")]
#[test_case::test_case(OutputFacts { is_terminal: true, no_color: true, journald: false }, "togi doctor: not ready: cpu\n"; "no colour")]
#[test_case::test_case(OutputFacts { is_terminal: false, no_color: false, journald: true }, "<3>\x1b[1;31mtogi doctor: not ready: cpu\x1b[0m\n"; "journald")]
fn the_failed_style_follows_the_terminal_and_the_environment(facts: OutputFacts, want: &str) {
    let name = format!("{facts:?}");
    let mut diagnosis = ready();
    diagnosis.ran[0].ok = false;
    let session = Session::new();
    let mut host = host_with(
        Diagnosis {
            ran: diagnosis.ran.clone(),
            skipped: Vec::new(),
            bios_context_error: None,
        },
        session.fs.clone(),
    );
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

#[test]
fn the_recorded_context_reaches_the_checks_only_for_a_journal_run_would_continue() {
    let session = Session::new();
    let host = host_with(ready(), session.fs.clone());
    assert_eq!(doctor(&host, session.path(), &[]).exit, Exit::Ok);
    let seen = host.diagnoses.borrow();
    assert_eq!(seen.as_slice(), [(Some(session.context), true)]);
    drop(seen);

    let older = Session::new();
    older.rewrite_first_line(
        &format!("\"ruleset\":{RULESET}"),
        &format!("\"ruleset\":{}", RULESET - 1),
    );
    let host = host_with(ready(), older.fs.clone());
    assert_eq!(doctor(&host, older.path(), &[]).exit, Exit::Ok);
    assert_eq!(host.diagnoses.borrow().as_slice(), [(None, true)]);

    let (_keep, absent) = missing_state();
    let host = host_with(ready(), Arc::new(MemFs::new()));
    assert_eq!(doctor(&host, &absent, &[]).exit, Exit::Ok);
    assert_eq!(host.diagnoses.borrow().as_slice(), [(None, true)]);
    assert!(!host.exists(&absent), "doctor created the state directory");
}

#[test_case::test_case("\"schema\":1", "\"schema\":99", &format!("(schema 99, ruleset {RULESET})"), "uses schema 1."; "newer schema")]
#[test_case::test_case(&format!("\"ruleset\":{RULESET}"), "\"ruleset\":99", "(schema 1, ruleset 99)", &format!("uses ruleset {RULESET}."); "newer ruleset")]
#[test_case::test_case("\"evidence\":1}", "\"evidence\":99}", &format!("(schema 1, ruleset {RULESET}, evidence epoch 99)"), "uses evidence epoch 1."; "newer evidence")]
fn a_journal_run_would_refuse_exits_16_before_any_lock_or_check(
    from: &str,
    to: &str,
    written: &str,
    uses: &str,
) {
    let name = from;
    let session = Session::new();
    session.rewrite_first_line(from, to);
    session.rewrite_first_line(
        "\"version\":\"0.0.0\"",
        "\"version\":\"0.0.0\",\"rev\":\"abc1234\"",
    );
    let before = (files(&session.fs, session.path()), session.text());
    let host = host_with(ready(), session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Incompatible, "{name}: {}", ran.err);
    assert_eq!(ran.out, "", "{name}");
    assert!(
            ran.err.starts_with(&format!(
                "togi doctor: this journal was written by togi 0.0.0+abc1234 {written}; this build, togi "
            )),
            "{name}: {}",
            ran.err
        );
    assert!(
            ran.err.ends_with(&format!(
                "{uses} Install togi 0.0.0 to continue this session, or run togi reset --all to archive it and start over.\n"
            )),
            "{name}: {}",
            ran.err
        );
    assert_eq!(
        (files(&session.fs, session.path()), session.text()),
        before,
        "{name}"
    );
    assert_eq!(host.calls.get(), 0, "{name}");
    assert_eq!(host.lock_acquisitions.get(), 0, "{name}");
}

#[test_case::test_case(true; "not json")]
#[test_case::test_case(false; "malformed later event")]
fn an_unreadable_journal_exits_1(rewrite: bool) {
    let name = format!("rewrite={rewrite}");
    let session = Session::new();
    if rewrite {
        session.fs.put(session.journal(), b"not json\n".to_vec());
    } else {
        let mut text = session.text();
        text.push_str("{\"seq\":\n");
        let at = text.find("\"kind\":\"config.loaded\"").unwrap();
        text.replace_range(at..at + 4, "\"kidn\"");
        session.fs.put(session.journal(), text.into_bytes());
    }
    let before = (files(&session.fs, session.path()), session.text());
    let host = host_with(ready(), session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error, "{name}: {}", ran.err);
    assert_eq!(ran.out, "", "{name}");
    assert_eq!(host.calls.get(), 0, "{name}");
    assert_eq!(
        (files(&session.fs, session.path()), session.text()),
        before,
        "{name}"
    );
    let causes = ran.err.matches("expected").count();
    assert!(causes <= 1, "{name}: a cause is repeated: {}", ran.err);
}

#[test]
fn the_default_config_comes_from_the_host() {
    let host = host_with(ready(), Arc::new(MemFs::new()));
    host.write(&host.config, "{").unwrap();
    let (_dir, state) = missing_state();
    let ran = doctor(&host, &state, &[]);
    assert_eq!(ran.exit, Exit::Usage, "{}", ran.err);
    assert!(
        ran.err.contains(&host.config.display().to_string()),
        "{}",
        ran.err
    );
}

#[test_case::test_case(true; "malformed")]
#[test_case::test_case(false; "missing")]
fn a_configuration_error_exits_2(malformed: bool) {
    let cfg = support::MemoryDir::new();
    let bad = cfg.path().join("bad.json");
    let fixture = Host::new();
    fixture.write(&bad, "{").unwrap();
    let missing = cfg.path().join("missing.json");
    let path = if malformed { &bad } else { &missing };
    let session = Session::with_fs(fixture.fs.clone(), togi_watchtest::Scenario::Controls);
    let host = host_with(ready(), fixture.fs);
    let ran = doctor(&host, session.path(), &["--config", path.to_str().unwrap()]);
    assert_eq!(ran.exit, Exit::Usage, "{}", ran.err);
    assert!(ran.err.contains(path.to_str().unwrap()), "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

#[test_case::test_case(false; "trailing")]
#[test_case::test_case(true; "leading")]
fn config_is_accepted_before_and_after_the_command(leading: bool) {
    let cfg = support::MemoryDir::new();
    let missing = cfg.path().join("missing.json");
    let missing = missing.to_str().unwrap();
    let args = if leading {
        ["--config", missing, "doctor"]
    } else {
        ["doctor", "--config", missing]
    };
    let session = Session::new();
    let host = host_with(ready(), session.fs.clone());
    let ran = host.run(session.path(), &args);
    assert_eq!(ran.exit, Exit::Usage, "{args:?}");
    assert!(ran.err.contains("missing.json"), "{args:?}: {}", ran.err);
}

#[test]
fn a_busy_host_lock_exits_3_only_for_root() {
    let session = Session::new();
    let host = host_with(ready(), session.fs.clone());
    let _held = host.locks.acquire(&host.lock).unwrap();
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    assert_eq!(
        ran.err,
        format!(
            "togi doctor: acquire host lock {}: another togi process holds the host lock\n",
            host.lock.display()
        )
    );
    assert_eq!(host.calls.get(), 0);

    let mut host = host_with(ready(), session.fs.clone());
    host.root = false;
    host.lock = host.lock.clone();
    let _held = host.locks.acquire(&host.lock).unwrap();
    assert_eq!(doctor(&host, session.path(), &[]).exit, Exit::Ok);
}

#[test]
fn real_fs_a_host_lock_that_cannot_be_opened_exits_1() {
    let mut host = Host::real();
    *host.diagnosis.borrow_mut() = Some(Ok(ready()));
    host.lock = host
        .lock
        .parent()
        .unwrap()
        .join("missing")
        .join("togi.lock");
    let ran = doctor(&host, &host.lock.with_file_name("state"), &[]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(
        ran.err,
        format!(
            "togi doctor: resolve the parent directory of host lock {}: No such file or directory (os error 2)\n",
            host.lock.display()
        )
    );
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn a_failed_diagnosis_exits_1() {
    let session = Session::new();
    let host = session.host();
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
    let host = host_with(diagnosis, session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(ran.out.starts_with("CHECK"), "{}", ran.out);
    assert!(!ran.out.contains("ready"), "{}", ran.out);
    assert_eq!(
        ran.err,
        "togi doctor: read BIOS context: the SMU driver needs Linux\n"
    );
}

#[test_case::test_case(false; "incompatible journal")]
#[test_case::test_case(true; "absent directory")]
fn another_platform_stops_before_the_journal_the_lock_and_every_check(missing: bool) {
    let session = Session::new();
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let (_keep, absent) = missing_state();
    let name = format!("missing={missing}");
    let state = if missing { &absent } else { session.path() };
    let mut host = host_with(ready(), session.fs.clone());
    host.linux = false;
    let held = host.locks.acquire(&host.lock).unwrap();
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
    assert!(
        !session.host().exists(&absent),
        "doctor created the state directory"
    );

    let mut host = host_with(ready(), session.fs.clone());
    host.linux = false;
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(host.lock_acquisitions.get(), 0, "the host lock was taken");
}

#[test]
fn a_closed_stdout_is_an_error() {
    let session = Session::new();
    let host = host_with(ready(), session.fs.clone());
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
    session.fs.put(session.journal(), text.into_bytes());
    let host = host_with(ready(), session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert!(
        ran.err.starts_with(&format!(
            "togi doctor: unknown kind \"future.fact\" in journal written by togi 0.0.0 (schema 1, ruleset {RULESET}); this build, togi "
        )),
        "{}",
        ran.err
    );
    assert!(
        ran.err.ends_with(&format!(
            " (schema 1, ruleset {RULESET}), cannot safely modify it; install the build that wrote it\n"
        )),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains("reset --all"), "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn the_unknown_kind_refusal_is_red_bold_on_a_terminal_like_the_stamp_refusal() {
    let session = Session::new();
    let text = format!(
        "{}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"future.fact\",\"msg\":\"x\",\"data\":{{}}}}\n",
        session.text(),
        session.kinds().len() + 1
    );
    session.fs.put(session.journal(), text.into_bytes());
    let mut host = host_with(ready(), session.fs.clone());
    host.facts = OutputFacts {
        is_terminal: true,
        ..OutputFacts::default()
    };
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(
        ran.err.starts_with("\x1b[1;31mtogi doctor: unknown kind"),
        "{:?}",
        ran.err
    );
    assert!(ran.err.ends_with("\x1b[0m\n"), "{:?}", ran.err);
}

#[test]
fn an_unprivileged_failed_check_exits_15_and_another_platform_exits_1() {
    let mut diagnosis = ready();
    let user = diagnosis
        .ran
        .iter_mut()
        .find(|check| check.name == "backend_user")
        .unwrap();
    user.ok = false;
    diagnosis.ran.retain(|check| check.name != "bios_context");
    let session = Session::new();
    let mut host = host_with(diagnosis, session.fs.clone());
    host.root = false;
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Preflight, "{}", ran.err);
    assert_eq!(ran.err, "togi doctor: not ready: backend_user\n");
    assert!(
        ran.out.ends_with("not ready: backend_user\n"),
        "{}",
        ran.out
    );
    assert_eq!(host.lock_acquisitions.get(), 0);

    let mut host = host_with(ready(), session.fs.clone());
    host.root = false;
    host.linux = false;
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(
        ran.err,
        "togi doctor: hardware runs need Linux: unsupported operation\n"
    );
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn a_current_journal_before_its_session_context_passes_no_context() {
    let session = Session::new();
    let text = session.text();
    let kept: Vec<&str> = text
        .lines()
        .take_while(|line| !line.contains("\"kind\":\"session.context\""))
        .collect();
    assert!(!kept.is_empty() && kept.len() < text.lines().count());
    session.fs.put(
        session.journal(),
        format!("{}\n", kept.join("\n")).into_bytes(),
    );
    let host = host_with(ready(), session.fs.clone());
    let ran = doctor(&host, session.path(), &[]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(host.diagnoses.borrow().as_slice(), [(None, true)]);
}

fn assert_snapshot(snapshot: &str, actual: &str) {
    let expected = snapshot
        .strip_prefix("---\n")
        .unwrap()
        .split_once("\n---\n")
        .unwrap()
        .1;
    assert_eq!(actual, expected);
}
