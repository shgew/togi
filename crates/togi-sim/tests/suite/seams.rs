// The seams of a simulated machine: ranking, validation, crashes, topology and the kernel log.

use jiff::SignedDuration;
use std::time::Duration;
use togi_machine::{
    BootId, CancellationToken, CcdId, CoreInfo, CoreRank, CpuId, Error, KernelCursor, Partial,
    Regime, ResetKind, Signal,
};
use togi_sim::definition::ScriptDef;
use togi_sim::{History, RunSettings};

use test_case::test_case;

use crate::common::{Rig, basic, core, flat_cores, machine, model, offset};

const MINUTE: Duration = Duration::from_secs(60);

fn two_cores() -> togi_sim::MachineDefinition {
    machine(2, vec![])
}

#[test]
fn ranking_is_unavailable_unless_the_machine_declares_one() {
    let mut rig = Rig::new(&two_cores(), 1);
    let Err(Partial { value, error }) = rig.machine.host.ranking() else {
        panic!("a ranking");
    };
    assert!(value.is_empty() && matches!(error, Error::Other(_)));

    let mut definition = two_cores();
    definition.ranking = Some(vec![11, 22]);
    let mut rig = Rig::new(&definition, 1);
    let ranking = rig.machine.host.ranking().expect("a ranking");
    assert_eq!(
        ranking,
        [
            CoreRank {
                core: core(0),
                value: 11
            },
            CoreRank {
                core: core(1),
                value: 22
            }
        ]
    );
    rig.sim.crash();
    assert!(matches!(
        rig.machine.host.ranking(),
        Err(Partial {
            error: Error::Crashed,
            ..
        })
    ));
}

#[test]
fn write_faults_start_counting_when_armed() {
    let mut rig = Rig::new(&two_cores(), 1);
    rig.sim.fail_write(2);
    rig.set(0, -1).expect("the first write");
    assert!(rig.set(1, -2).is_err(), "the second write fails");
    assert_eq!(
        rig.read(1).expect("read").0,
        0,
        "the failed write was not applied"
    );
    rig.sim.crash_at_write(1);
    assert!(matches!(rig.set(1, -2), Err(Error::Crashed)));
    rig.sim.reboot();
}

#[test]
fn a_missing_backend_fails_every_start_of_its_workloads() {
    let mut rig = Rig::new(&two_cores(), 1);
    let mprime = basic(Regime::R1, &[0], MINUTE);
    assert_eq!(mprime.workload.backend(), togi_machine::Backend::Mprime);
    rig.sim.miss_backend(togi_machine::Backend::Mprime);
    let Err(failed) = rig.start(mprime) else {
        panic!("started");
    };
    assert!(matches!(failed.error, Error::BackendMissing));
    assert!(failed.value.is_some(), "nothing runs after a failed start");
    let other = crate::common::spec(
        "0002",
        Regime::R1,
        togi_machine::pick_workload(Regime::R1, 1),
        &[0],
        MINUTE,
    );
    assert!(rig.start(other).is_ok(), "the other backend still starts");
}

#[test_case(ResetKind::PowerLoss; "power loss")]
#[test_case(ResetKind::PowerButton; "power button")]
#[test_case(ResetKind::ThermalTrip; "thermal trip")]
fn an_old_kernel_reports_no_reset_reason_whatever_the_reset(kind: ResetKind) {
    let mut definition = two_cores();
    definition.old_kernel = true;
    let mut rig = Rig::new(&definition, 1);
    rig.sim.next_reset(kind);
    rig.sim.crash();
    rig.sim.reboot();
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert!(
        reason.kind.is_none() && !reason.supported && reason.raw.is_empty(),
        "{reason:?}"
    );
}

#[test]
fn only_identity_checks_block_smu_validation_and_the_watchdog_fails_separately() {
    let mut rig = Rig::new(&two_cores(), 1);
    for name in ["cpu", "ryzen_smu"] {
        rig.sim.fail_check(name, "unsupported");
        let error = rig.machine.host.validate_smu().expect_err("a refusal");
        assert_eq!(error.to_string(), format!("validate {name}: unsupported"));
        rig.sim.fail_check(name, "");
    }
    rig.sim.fail_check("root", "not root");
    assert!(rig.machine.host.validate_smu().is_ok());
    for detail in ["not armed", ""] {
        rig.sim.fail_check("watchdog", detail);
        let check = rig.machine.host.watchdog();
        assert_eq!(check.name, "watchdog");
        assert_eq!(check.ok, detail.is_empty());
        if !detail.is_empty() {
            assert_eq!(check.detail, detail);
        }
    }
}

#[test]
fn an_smu_access_to_a_missing_core_changes_nothing() {
    let mut definition = two_cores();
    definition.bios = Some(vec![-3, -4]);
    let mut rig = Rig::new(&definition, 1);
    let missing = core(2);
    let read = rig.machine.smu.offset(missing).expect_err("no such core");
    assert!(read.to_string().contains("no core"));
    let write = rig
        .machine
        .smu
        .set_offset(missing, offset(-10))
        .expect_err("no such core");
    assert!(write.to_string().contains("no core"));
    assert_eq!(rig.read(0).expect("read").0, -3);
    assert_eq!(rig.read(1).expect("read").0, -4);
}

#[test]
fn a_cancelled_start_does_not_consume_a_setup_fault_or_move_the_clock() {
    let mut rig = Rig::new(&two_cores(), 1);
    rig.sim.fail_setup(1);
    let start = rig.sim.now();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ready = rig.proof();
    let cancelled = rig
        .machine
        .trials
        .start(&cancel, basic(Regime::R1, &[0], MINUTE), ready);
    assert!(matches!(
        cancelled,
        Err(Partial {
            error: Error::Cancelled,
            ..
        })
    ));
    let Err(setup) = rig.start(basic(Regime::R1, &[0], MINUTE)) else {
        panic!("the setup fault was consumed");
    };
    assert!(setup.error.to_string().contains("setup failure"));
    assert_eq!(rig.sim.now(), start);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Machine {
    Running,
    Crashed,
    Rebooted,
}

#[test_case(Machine::Running; "running")]
#[test_case(Machine::Crashed; "crashed")]
#[test_case(Machine::Rebooted; "rebooted")]
fn stopping_a_trial_matches_what_a_cancelled_wait_would_clean_up(state: Machine) {
    let mut rig = Rig::new(&two_cores(), 1);
    let mut running = rig
        .start(basic(Regime::R1, &[0], MINUTE))
        .unwrap_or_else(|error| panic!("{error}"));
    match state {
        Machine::Running => {}
        Machine::Crashed => rig.sim.crash(),
        Machine::Rebooted => rig.sim.reboot(),
    }
    let before = rig.sim.now();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let waited = running.wait(&cancel, &mut crate::common::Recorder::default());
    let stopped = running.stop();
    if state == Machine::Running {
        assert!(
            matches!(
                &waited,
                Err(Partial {
                    error: Error::Cancelled,
                    ..
                })
            ),
            "{waited:?}"
        );
        assert!(stopped.is_ok(), "{stopped:?}");
    } else {
        assert!(
            matches!(
                &waited,
                Err(Partial {
                    error: Error::Crashed,
                    ..
                })
            ),
            "{waited:?}"
        );
        assert!(matches!(stopped, Err(Error::Crashed)), "{stopped:?}");
    }
    assert_eq!(rig.sim.now(), before);
}

#[test]
fn a_crashed_machine_rejects_its_operations_and_does_not_move_its_clock() {
    let mut rig = Rig::new(&two_cores(), 1);
    let boot = rig.boot();
    rig.sim.crash();
    let start = rig.sim.now();
    let cancel = CancellationToken::new();
    let ready = rig.proof();
    let machine = &mut rig.machine;
    let results: Vec<(&str, Result<(), Error>)> = vec![
        (
            "sleep",
            machine.clock.sleep(&cancel, Duration::from_secs(3600)),
        ),
        ("offset", machine.smu.offset(core(0)).map(drop)),
        ("write", machine.smu.set_offset(core(0), offset(-10))),
        ("write all", machine.smu.set_all_offsets(offset(-10))),
        ("boot", machine.host.boot_id().map(drop)),
        ("topology", machine.host.topology().map(drop)),
        ("bios", machine.host.bios_context().map(drop)),
        (
            "mces",
            machine
                .kernel
                .mces(&boot, Duration::ZERO)
                .map(drop)
                .map_err(|partial| partial.error),
        ),
        ("reset", machine.kernel.reset_reason(&boot).map(drop)),
        (
            "reset after",
            machine.kernel.reset_reason_after(&boot).map(drop),
        ),
        ("pstore", machine.kernel.saved_pstore(&boot).map(drop)),
        (
            "start",
            machine
                .trials
                .start(&cancel, basic(Regime::R1, &[0], MINUTE), ready)
                .map(drop)
                .map_err(|partial| partial.error),
        ),
        (
            "read cursor",
            machine
                .kernel
                .read_mces(&boot, None)
                .map(drop)
                .map_err(|partial| partial.error),
        ),
    ];
    for (name, result) in results {
        assert!(matches!(result, Err(Error::Crashed)), "{name}: {result:?}");
    }
    assert_eq!(rig.sim.now(), start);
    rig.sim.reboot();
    assert_eq!(
        rig.read(0).expect("read").0,
        0,
        "a crashed write changed nothing"
    );
}

#[test_case(2; "two cores")]
#[test_case(4; "four cores")]
#[test_case(16; "sixteen cores")]
fn the_topology_splits_the_cores_into_two_ccds_with_their_sibling_threads(cores: usize) {
    let definition = machine(i64::try_from(cores).expect("count"), vec![]);
    let mut rig = Rig::new(&definition, 1);
    let want: Vec<CoreInfo> = (0..cores)
        .map(|index| CoreInfo {
            core: togi_machine::CoreId::try_from(index).expect("core"),
            ccd: CcdId(u8::try_from(index / (cores / 2)).expect("ccd")),
            cpus: vec![
                CpuId(u16::try_from(index).expect("cpu")),
                CpuId(u16::try_from(index + cores).expect("cpu")),
            ],
        })
        .collect();
    assert_eq!(rig.machine.host.topology().expect("topology"), want);
    assert_eq!(rig.sim.monotonic(), Duration::ZERO);
}

#[test]
fn a_machine_starts_at_the_epoch_or_one_reboot_after_its_history() {
    let rig = Rig::new(&two_cores(), 1);
    let epoch = jiff::Timestamp::from_second(1_767_225_600).expect("a time");
    assert_eq!(rig.sim.now(), epoch);
    let last = epoch + SignedDuration::from_hours(7);
    let settings = RunSettings {
        history: History {
            last_time: Some(last),
            ..History::default()
        },
        ..RunSettings::default()
    };
    let rig = Rig::with(model(&two_cores()), settings);
    assert_eq!(rig.sim.now(), last + SignedDuration::from_secs(90));
}

#[test_case(false; "fresh")]
#[test_case(true; "resumed")]
fn the_clock_seam_follows_sleeps_wall_jumps_and_reboots_from_a_fresh_or_resumed_start(
    resumed: bool,
) {
    let epoch = jiff::Timestamp::from_second(1_767_225_600).expect("a time");
    let last = epoch + SignedDuration::from_hours(7);
    let (settings, start) = if resumed {
        let settings = RunSettings {
            history: History {
                last_time: Some(last),
                ..History::default()
            },
            ..RunSettings::default()
        };
        (settings, last + SignedDuration::from_secs(90))
    } else {
        (RunSettings::default(), epoch)
    };
    let mut rig = Rig::with(model(&two_cores()), settings);
    let cancel = CancellationToken::new();
    assert_eq!(rig.machine.clock.now(), start);
    assert_eq!(rig.machine.clock.monotonic(), Duration::ZERO);
    rig.machine
        .clock
        .sleep(&cancel, Duration::from_secs(5))
        .expect("sleep");
    assert_eq!(
        rig.machine.clock.now(),
        start + SignedDuration::from_secs(5)
    );
    assert_eq!(rig.machine.clock.monotonic(), Duration::from_secs(5));
    rig.sim.jump_wall(SignedDuration::from_hours(1));
    assert_eq!(
        rig.machine.clock.now(),
        start + SignedDuration::from_secs(5) + SignedDuration::from_hours(1)
    );
    assert_eq!(rig.machine.clock.monotonic(), Duration::from_secs(5));
    let before = rig.machine.clock.now();
    rig.sim.crash();
    rig.sim.reboot();
    assert_eq!(
        rig.machine.clock.now(),
        before + SignedDuration::from_secs(90)
    );
    assert_eq!(rig.machine.clock.monotonic(), Duration::ZERO);
    assert_eq!(rig.machine.clock.now(), rig.sim.now());
}

fn cursor_machine() -> Rig {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    definition.script = vec![ScriptDef {
        trial: "0001".to_owned(),
        signal: Some(Signal::CorrectedMce),
        at_s: 7.0,
        ..ScriptDef::default()
    }];
    Rig::new(&definition, 1)
}

#[test]
fn a_kernel_cursor_resumes_across_empty_reads_and_stays_in_its_boot() {
    let mut rig = cursor_machine();
    let boot = rig.boot();
    let first = rig.machine.kernel.read_mces(&boot, None).expect("a read");
    assert!(first.mces.is_empty());
    rig.pass(basic(Regime::R1, &[0], Duration::from_secs(10)));
    let read = rig
        .machine
        .kernel
        .read_mces(&boot, Some(&first.cursor))
        .expect("a read");
    assert_eq!(read.mces.len(), 1);
    assert_eq!(read.mces[0].monotonic, Duration::from_secs(7));
    let empty = rig
        .machine
        .kernel
        .read_mces(&boot, Some(&read.cursor))
        .expect("a read");
    assert!(empty.mces.is_empty());
    assert_eq!(empty.cursor, read.cursor, "an empty read leaves the cursor");

    rig.sim.reboot();
    let next = rig.boot();
    let foreign = rig.machine.kernel.read_mces(&next, Some(&read.cursor));
    assert!(matches!(
        foreign,
        Err(Partial {
            error: Error::CursorMissing,
            ..
        })
    ));
    let next_read = rig.machine.kernel.read_mces(&next, None).expect("a read");
    assert!(next_read.mces.is_empty() && next_read.cursor != read.cursor);
    let tail = rig
        .machine
        .kernel
        .read_mces(&boot, Some(&first.cursor))
        .expect("an old boot's log persists");
    assert_eq!(tail.mces.len(), 1);
}

#[test]
fn unknown_boots_and_cursors_are_named_errors() {
    let mut rig = Rig::new(&two_cores(), 1);
    let boot = rig.boot();
    let missing = BootId::from("missing".to_owned());
    assert!(matches!(
        rig.machine.kernel.read_mces(&missing, None),
        Err(Partial {
            error: Error::BootMissing,
            ..
        })
    ));
    assert!(matches!(
        rig.machine.kernel.mces(&missing, Duration::ZERO),
        Err(Partial {
            error: Error::BootMissing,
            ..
        })
    ));
    assert!(matches!(
        rig.machine.kernel.reset_reason(&missing),
        Err(Error::BootMissing)
    ));
    for old in [&missing, &boot] {
        let after = rig.machine.kernel.reset_reason_after(old);
        assert!(matches!(after, Err(Error::BootMissing)), "{old}: {after:?}");
    }
    for suffix in ["no-number", "-1", "1"] {
        let cursor = KernelCursor::from(format!("{boot}:{suffix}"));
        assert!(
            matches!(
                rig.machine.kernel.read_mces(&boot, Some(&cursor)),
                Err(Partial {
                    error: Error::CursorMissing,
                    ..
                })
            ),
            "{suffix}"
        );
    }
    rig.sim.next_reset(ResetKind::ThermalTrip);
    rig.sim.crash();
    rig.sim.reboot();
    rig.sim.power_loss();
    rig.sim.reboot();
    let after = rig
        .machine
        .kernel
        .reset_reason_after(&boot)
        .expect("a successor");
    assert_eq!(after.kind, Some(ResetKind::ThermalTrip));
}

#[test_case(ResetKind::Watchdog, Some("hardware watchdog timer expired"); "watchdog")]
#[test_case(ResetKind::SyncFlood, Some("an uncorrected error caused a data fabric sync flood event"); "sync flood")]
#[test_case(ResetKind::CpuShutdown, Some("internal CPU shutdown event occurred"); "cpu shutdown")]
#[test_case(ResetKind::PowerButton, Some("power button was pressed for 4 seconds"); "power button")]
#[test_case(ResetKind::ThermalTrip, Some("thermal pin BP_THERMTRIP_L was tripped"); "thermal trip")]
#[test_case(ResetKind::Unknown, Some("unrecognized reset reason"); "unknown")]
#[test_case(ResetKind::PowerLoss, None; "power loss")]
fn each_reset_leaves_the_line_a_recent_kernel_logs(kind: ResetKind, line: Option<&str>) {
    let mut rig = Rig::new(&two_cores(), 1);
    rig.sim.next_reset(kind);
    rig.sim.crash();
    rig.sim.reboot();
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert!(reason.supported);
    let expected = if kind == ResetKind::PowerLoss {
        None
    } else {
        Some(kind)
    };
    assert_eq!(reason.kind, expected);
    match line {
        Some(text) => assert!(
            reason.raw.iter().any(|raw| raw.contains(text)),
            "{:?}",
            reason.raw
        ),
        None => assert!(reason.raw.is_empty(), "{:?}", reason.raw),
    }
}

#[test]
fn a_boot_without_a_reset_reports_no_kind_and_a_hold_of_the_power_button_is_its_own() {
    let mut rig = Rig::new(&two_cores(), 1);
    let first = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&first).expect("a reason");
    assert!(reason.supported && reason.kind.is_none() && reason.raw.is_empty());
    rig.sim.hold_power_button();
    rig.sim.reboot();
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert_eq!(reason.kind, Some(ResetKind::PowerButton));
}

#[test]
fn saved_pstore_is_always_absent() {
    let mut rig = Rig::new(&two_cores(), 1);
    let boot = rig.boot();
    assert!(
        rig.machine
            .kernel
            .saved_pstore(&boot)
            .expect("a pstore")
            .is_none()
    );
}
