//! The day command refuses missing sessions and shares existing writer/hardware exclusion.

#![cfg(test)]

use crate::support;

use jiff::Timestamp;
use std::path::Path;
use togi_cli::Exit;
use togi_cli::system::System;
use togi_journal::{SessionId, SessionStart, Stamp};

fn seed(host: &support::Host) {
    let dir = host.lock_state_dir(Path::new("/state")).unwrap();
    let mut journal = dir
        .open_at("seed".to_owned().into(), Timestamp::UNIX_EPOCH)
        .unwrap();
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: None,
            },
            SessionStart {
                build: togi_cli::report::current_build(),
                session: SessionId::new(Timestamp::UNIX_EPOCH, None).unwrap(),
                cores: host.topology.clone(),
            }
            .into(),
            &[],
        )
        .unwrap();
    journal.close().unwrap();
}

#[test]
fn no_session_refuses_without_creating_any_state() {
    let host = support::Host::new();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(ran.err.contains("no session journal"), "{}", ran.err);
    assert!(!host.exists("/state"));
    assert_eq!(host.lock_acquisitions.get(), 0);
}

#[test]
fn day_refuses_a_busy_host_before_opening_hardware() {
    let host = support::Host::new();
    seed(&host);
    let _held = host.acquire_host_lock().unwrap();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Locked);
    assert!(ran.err.contains("lock"), "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn day_refuses_a_busy_journal_before_opening_hardware() {
    let host = support::Host::new();
    seed(&host);
    let _held = host.lock_state_dir(Path::new("/state")).unwrap();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Locked);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn status_remains_lock_free_while_day_owns_both_locks() {
    let host = support::Host::new();
    seed(&host);
    let _hardware = host.acquire_host_lock().unwrap();
    let _writer = host.lock_state_dir(Path::new("/state")).unwrap();
    let before = host.lock_acquisitions.get();
    let ran = host.run(Path::new("/state"), &["status"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(host.lock_acquisitions.get(), before);
}

#[test]
fn destination_opens_the_existing_journal_without_starting_another_session() {
    let host = support::Host::new();
    seed(&host);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut context = togi_cli::cli::Context {
        state_dir: Path::new("/state"),
        out: &mut out,
        err: &mut err,
        system: &host,
    };
    let journal = togi_cli::day_destination::open(
        &mut context,
        "day".to_owned().into(),
        &togi_cli::report::current_build(),
    )
    .unwrap();
    assert_eq!(journal.events().len(), 1);
    assert!(journal.session().is_some());
    journal.close().unwrap();
}

#[test_case::test_case(&["run", "--no-tui"] ; "run")]
#[test_case::test_case(&["reset", "--all"] ; "reset")]
fn tuning_writers_cannot_start_while_day_owns_both_locks(args: &[&str]) {
    let host = support::Host::new();
    seed(&host);
    let _hardware = host.acquire_host_lock().unwrap();
    let _writer = host.lock_state_dir(Path::new("/state")).unwrap();
    let ran = host.run(Path::new("/state"), args);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

fn day(session: &support::Session) -> support::Ran {
    session.host().run(session.path(), &["day"])
}

/// Appends an event after the session's last one, the way a later or different build would.
fn append_line(session: &support::Session, kind: &str, data: &str) -> String {
    let text = format!(
        "{}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"{kind}\",\"msg\":\"x\",\"data\":{data}}}\n",
        session.text(),
        session.kinds().len() + 1
    );
    session.fs.put(session.journal(), text.clone().into_bytes());
    text
}

fn assert_refused_untouched(session: &support::Session, before: &str, ran: &support::Ran) {
    assert_eq!(ran.out, "");
    assert_eq!(session.text(), before);
    assert!(
        session.kinds().iter().all(|kind| !kind.starts_with("day.")),
        "{:?}",
        session.kinds()
    );
}

#[test_case::test_case("\"ruleset\":", togi_tuner::RULESET, 0, "uses ruleset", "togi run archives it"; "older ruleset")]
#[test_case::test_case("\"ruleset\":", togi_tuner::RULESET, togi_tuner::RULESET + 1, "uses ruleset", "Install togi"; "newer ruleset")]
#[test_case::test_case("\"schema\":", 1, 2, "uses schema", "Install togi"; "newer schema")]
fn a_journal_stamped_by_another_build_is_refused_as_incompatible_and_left_alone(
    field: &str,
    recorded: u32,
    written: u32,
    uses: &str,
    advice: &str,
) {
    let session = support::Session::new();
    session.rewrite_first_line(&format!("{field}{recorded}"), &format!("{field}{written}"));
    let before = session.text();
    let ran = day(&session);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(
        ran.err
            .starts_with("togi day: this journal was written by togi "),
        "{}",
        ran.err
    );
    assert!(ran.err.contains(uses), "{}", ran.err);
    assert!(ran.err.contains(advice), "{}", ran.err);
    assert_refused_untouched(&session, &before, &ran);
}

#[test]
fn a_journal_with_an_unknown_kind_is_refused_as_incompatible_and_left_alone() {
    let session = support::Session::new();
    let before = append_line(&session, "future.fact", "{}");
    let ran = day(&session);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(
        ran.err
            .starts_with("togi day: unknown kind \"future.fact\" in journal written by togi "),
        "{}",
        ran.err
    );
    assert!(
        ran.err
            .ends_with("cannot safely modify it; install the build that wrote it\n"),
        "{}",
        ran.err
    );
    assert_refused_untouched(&session, &before, &ran);
}

#[test]
fn an_archived_session_is_refused_and_left_alone() {
    let session = support::Session::new();
    let before = append_line(
        &session,
        "session.archived",
        "{\"session\":\"20260101T000000Z\"}",
    );
    let ran = day(&session);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(
        ran.err,
        "togi day: the session is archived; start a new session with togi run\n"
    );
    assert_refused_untouched(&session, &before, &ran);
}

mod record {
    use super::*;
    use togi_journal::{
        CrashedBoot, DayBootStart, DayCheckpoint, DayRecord, DayUncleanEnd, OffsetSource, Running,
    };
    use togi_machine::{
        BiosContext, CcdId, CoreId, CoreInfo, CpuId, EndEvidence, ObservedOffset, PerCore,
        ResetReason,
    };

    const STATE: &str = "/state";
    const ENTERED: &str = "2026-10-19T12:00:00Z";

    fn core(index: i64) -> CoreId {
        CoreId::new(index).unwrap()
    }

    fn profile(values: [i64; 4]) -> PerCore<ObservedOffset> {
        (0..4)
            .zip(values)
            .map(|(index, value)| (core(index), ObservedOffset(value)))
            .collect()
    }

    /// Four cores, two on each of two CCDs, and one boot, `crashed`, whose observer was last read back at
    /// second 700 and whose unclean end boot `next` journaled.
    fn host() -> support::Host {
        let mut host = support::Host::new();
        host.topology = (0..4)
            .map(|index| CoreInfo {
                core: core(index),
                ccd: CcdId::new(index / 2).unwrap(),
                cpus: vec![CpuId::from(u16::try_from(index).unwrap())],
            })
            .collect();
        let dir = host.lock_state_dir(Path::new(STATE)).unwrap();
        let mut journal = dir
            .open_at("seed".to_owned().into(), Timestamp::UNIX_EPOCH)
            .unwrap();
        let stamp = |second: i64| Stamp {
            time: Timestamp::from_second(second).unwrap(),
            mono: None,
        };
        journal
            .append(
                stamp(0),
                SessionStart {
                    build: togi_cli::report::current_build(),
                    session: SessionId::new(Timestamp::UNIX_EPOCH, None).unwrap(),
                    cores: host.topology.clone(),
                }
                .into(),
                &[],
            )
            .unwrap();
        journal
            .append(
                stamp(100),
                DayBootStart {
                    build: togi_cli::report::current_build(),
                    bios: BiosContext::default(),
                    offsets: profile([-5, -5, -10, -10]),
                    watchdog: None,
                }
                .into(),
                &[],
            )
            .unwrap();
        journal
            .append(
                stamp(700),
                DayCheckpoint {
                    applied_secs: 600,
                    load_mix: togi_journal::LoadMix::default(),
                    watchdog: None,
                }
                .into(),
                &[],
            )
            .unwrap();
        journal
            .append(
                stamp(1000),
                DayUncleanEnd {
                    boot: "seed".to_owned().into(),
                    last_checkpoint: None,
                    offsets: profile([-5, -5, -10, -10]),
                    reset_reason: ResetReason {
                        kind: None,
                        raw: Vec::new(),
                        supported: false,
                    },
                    evidence: EndEvidence::unknown(),
                }
                .into(),
                &[],
            )
            .unwrap();
        journal.close().unwrap();
        host.boot = "next".to_owned();
        host
    }

    fn events(host: &support::Host) -> Vec<togi_journal::Event> {
        host.recording(&Path::new(STATE).join("events.jsonl"))
            .unwrap()
            .events()
            .to_vec()
    }

    fn records(host: &support::Host) -> Vec<DayRecord> {
        events(host)
            .into_iter()
            .filter_map(|event| {
                if let togi_journal::Payload::DayRecord(record) = event.payload {
                    Some(record)
                } else {
                    None
                }
            })
            .collect()
    }

    fn record(host: &support::Host, args: &[&str]) -> support::Ran {
        let mut all = vec!["day", "record"];
        all.extend_from_slice(args);
        host.run(Path::new(STATE), &all)
    }

    #[test]
    fn a_boot_recorded_without_offsets_carries_the_read_back_and_when() {
        let host = host();
        let ran = record(
            &host,
            &["--boot", "seed", "--running", "build", "--note", "nix"],
        );
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        let [record] = records(&host).try_into().unwrap();
        assert_eq!(
            record.boot,
            CrashedBoot::UncleanEnd {
                boot: "seed".to_owned().into()
            }
        );
        assert_eq!(
            record.source,
            OffsetSource::ReadBack {
                at: Timestamp::from_second(700).unwrap()
            }
        );
        assert_eq!(record.running, Running::Build);
        assert_eq!(record.note.as_deref(), Some("nix"));
        let values: Vec<_> = record.offsets.get().iter().map(|(_, o)| o.0).collect();
        assert_eq!(values, [-5, -5, -10, -10]);
    }

    #[test]
    fn the_effect_is_printed_before_the_record_is_appended() {
        let host = host();
        let ran = record(&host, &["--boot", "seed"]);
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        let effect = ran.out.find("whole-profile veto").expect(&ran.out);
        let appended = ran.out.find("crash recorded as event").expect(&ran.out);
        assert!(effect < appended, "{}", ran.out);
    }

    #[test]
    fn a_crash_before_the_observer_ran_carries_entered_offsets_and_when_they_were_entered() {
        let host = host();
        let ran = record(
            &host,
            &[
                "--unobserved-at",
                "2026-10-19T11:00:00Z",
                "--offset",
                "ccd0=-25",
                "--offset",
                "ccd1=-30",
                "--entered-at",
                ENTERED,
                "--running",
                "desktop",
            ],
        );
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        let [record] = records(&host).try_into().unwrap();
        assert_eq!(
            record.boot,
            CrashedBoot::Unobserved {
                at: "2026-10-19T11:00:00Z".parse().unwrap()
            }
        );
        assert_eq!(
            record.source,
            OffsetSource::Entered {
                at: ENTERED.parse().unwrap()
            }
        );
        let values: Vec<_> = record.offsets.get().iter().map(|(_, o)| o.0).collect();
        assert_eq!(values, [-25, -25, -30, -30]);
    }

    #[test]
    fn a_record_that_names_no_offsets_is_refused_and_appends_nothing() {
        let host = host();
        let before = events(&host).len();
        let ran = record(&host, &["--unobserved-at", "2026-10-19T11:00:00Z"]);
        assert_eq!(ran.exit, Exit::Usage, "{}", ran.err);
        assert!(ran.err.contains("--offset"), "{}", ran.err);
        assert_eq!(events(&host).len(), before);
    }

    #[test]
    fn a_record_with_offsets_that_name_no_core_is_refused_at_the_terminal_too() {
        let host = host();
        host.terminal.set(true);
        host.typed
            .borrow_mut()
            .extend(["none", "2026-10-19T11:00:00Z", ""].map(str::to_owned));
        let before = events(&host).len();
        let ran = record(&host, &[]);
        assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
        assert!(ran.err.contains("input ended"), "{}", ran.err);
        assert_eq!(events(&host).len(), before);
    }

    #[test]
    fn a_boot_with_no_recorded_unclean_end_is_refused() {
        let host = host();
        let ran = record(&host, &["--boot", "elsewhere"]);
        assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
        assert!(ran.err.contains("no recorded unclean end"), "{}", ran.err);
        assert!(records(&host).is_empty());
    }

    #[test]
    fn a_boot_is_recorded_once() {
        let host = host();
        assert_eq!(record(&host, &["--boot", "seed"]).exit, Exit::Ok);
        let ran = record(&host, &["--boot", "seed"]);
        assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
        assert!(
            ran.err.contains("already has a crash record"),
            "{}",
            ran.err
        );
        assert_eq!(records(&host).len(), 1);
    }

    #[test]
    fn without_a_terminal_the_crash_must_be_named() {
        let host = host();
        let ran = record(&host, &[]);
        assert_eq!(ran.exit, Exit::Usage, "{}", ran.err);
        assert!(ran.err.contains("--boot or --unobserved-at"), "{}", ran.err);
    }

    #[test]
    fn recording_needs_root() {
        let mut host = host();
        host.root = false;
        let ran = record(&host, &["--boot", "seed"]);
        assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
        assert!(ran.err.contains("needs root"), "{}", ran.err);
        assert!(records(&host).is_empty());
    }

    #[test]
    fn recording_waits_for_the_writer_lock() {
        let host = host();
        let _held = host.lock_state_dir(Path::new(STATE)).unwrap();
        let ran = record(&host, &["--boot", "seed"]);
        assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    }

    #[test]
    fn the_terminal_and_the_flags_journal_the_same_record() {
        let by_flags = host();
        assert_eq!(
            record(
                &by_flags,
                &[
                    "--unobserved-at",
                    "2026-10-19T11:00:00Z",
                    "--offset",
                    "all=-20",
                    "--offset",
                    "core3=-10",
                    "--entered-at",
                    ENTERED,
                    "--running",
                    "build",
                    "--note",
                    "nix build",
                ],
            )
            .exit,
            Exit::Ok
        );
        let by_terminal = host();
        by_terminal.terminal.set(true);
        by_terminal.typed.borrow_mut().extend(
            [
                "none",
                "2026-10-19T11:00:00Z",
                "all=-20 core3=-10",
                ENTERED,
                "build",
                "nix build",
                "y",
            ]
            .map(str::to_owned),
        );
        let ran = record(&by_terminal, &[]);
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        assert_eq!(records(&by_terminal), records(&by_flags));
    }

    #[test]
    fn declining_the_confirmation_appends_nothing() {
        let host = host();
        host.terminal.set(true);
        host.typed
            .borrow_mut()
            .extend(["1", "", "", "", "n"].map(str::to_owned));
        let ran = record(&host, &[]);
        assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
        assert!(ran.out.contains("whole-profile veto"), "{}", ran.out);
        assert!(records(&host).is_empty());
    }

    #[test]
    fn the_unclean_end_is_the_default_choice_at_the_terminal() {
        let host = host();
        host.terminal.set(true);
        host.typed
            .borrow_mut()
            .extend(["", "", "", "", ""].map(str::to_owned));
        let ran = record(&host, &[]);
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        assert!(ran.err.contains("seed"), "{}", ran.err);
        let [record] = records(&host).try_into().unwrap();
        assert!(matches!(record.boot, CrashedBoot::UncleanEnd { .. }));
        assert!(matches!(record.source, OffsetSource::ReadBack { .. }));
    }
}
