use test_case::test_case;
use togi_machine::{
    BootId, EndEvidence, GraphicsDriver, KernelFault, KernelTail, MAX_FAULTS, MceCount, SleepMode,
};

use super::reset::{boots, boundary};
use super::support::{LATER, OLD, argv, boot, exit, kernel, kernel_lines, lines, locked, ok};
use crate::runner::RunError;
use crate::tail::{scan, sleep_end};

const RING: &str =
    "amdgpu 0000:03:00.0: amdgpu: ring gfx_0.0.0 timeout, signaled seq=1234, emitted seq=1236";
const AMD_RESET: &str = "amdgpu 0000:03:00.0: amdgpu: GPU reset begin!. Source:  1";
const AMD_DRM_TIMEOUT: &str = "[drm:amdgpu_job_timedout [amdgpu]] *ERROR* ring gfx_0.0.0 timeout, signaled seq=1234, emitted seq=1236";
const AMD_SOFT: &str = "amdgpu 0000:03:00.0: amdgpu: ring comp_1.0.0 timeout, but soft recovered";
const AMD_RING_RESET: &str = "amdgpu 0000:03:00.0: amdgpu: Starting gfx_0.0.0 ring reset";
const I915_CONTEXT: &str = "i915 0000:00:02.0: [drm] Xorg[1234] context reset due to GPU hang";
const XE_TILE_TIMEOUT: &str = "xe 0000:03:00.0: [drm] Tile0: GT0: Timedout job: seqno=123, lrc_seqno=123, guc_id=3, flags=0x0 in Xorg [1234]";
const XE_ENGINE_RESET: &str = "xe 0000:03:00.0: [drm] GT0: Engine reset: engine_class=rcs, logical_mask: 0x1, guc_id=3, state=0x0";
const XID: &str = "NVRM: Xid (0000:03:00): 79, Channel 00000001";
const XE_TIMEOUT: &str = "xe 0000:03:00.0: [drm] GT0: Timedout job: seqno=123, lrc_seqno=123, guc_id=3, flags=0x0 in Xorg [1234]";
const I915_HANG: &str = "i915 0000:00:02.0: [drm] GPU HANG: ecode 9:1:85dffffb, in Xorg [1234]";
const I915_RESET: &str =
    "i915 0000:00:02.0: [drm] GT0: Resetting chip for stopped heartbeat on rcs0";
const XE_RESET: &str = "xe 0000:03:00.0: [drm] GT0: reset started";
const OOPS: &str =
    "Oops: general protection fault, probably for non-canonical address 0xdead [#1] SMP NOPTI";
const HUNG: &str = "INFO: task kworker/0:1:123 blocked for more than 122 seconds.";
const UNCORRECTED: &str = "[Hardware Error]: Uncorrected, software restartable error.";
const CORRECTED: &str = "[Hardware Error]: Corrected error, no action required.";
const FATAL: &str = "mce: Fatal machine check";
const CALM: &str = "usb 1-2: new high-speed USB device number 5 using xhci_hcd";
const NO_BOOT: &str = "No journal boot entry found";

fn texts(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

const AMD_TIMEOUT: KernelFault = KernelFault::GraphicsTimeout {
    driver: GraphicsDriver::Amdgpu,
};

fn only(fault: KernelFault, logging_continued: bool) -> KernelTail {
    KernelTail::Faults {
        faults: vec![fault],
        logging_continued,
    }
}

#[test_case(&[RING], KernelFault::GraphicsTimeout { driver: GraphicsDriver::Amdgpu }; "amdgpu ring timeout")]
#[test_case(&[AMD_RESET], KernelFault::GraphicsReset { driver: GraphicsDriver::Amdgpu }; "amdgpu gpu reset")]
#[test_case(&[XID], KernelFault::NvidiaXid { code: 79 }; "nvidia xid")]
#[test_case(&[AMD_DRM_TIMEOUT], KernelFault::GraphicsTimeout { driver: GraphicsDriver::Amdgpu }; "amdgpu drm error form")]
#[test_case(&[AMD_SOFT], KernelFault::GraphicsTimeout { driver: GraphicsDriver::Amdgpu }; "amdgpu soft recovered timeout")]
#[test_case(&[AMD_RING_RESET], KernelFault::GraphicsReset { driver: GraphicsDriver::Amdgpu }; "amdgpu ring reset")]
#[test_case(&[I915_CONTEXT], KernelFault::GraphicsTimeout { driver: GraphicsDriver::I915 }; "i915 context reset due to hang")]
#[test_case(&[XE_TILE_TIMEOUT], KernelFault::GraphicsTimeout { driver: GraphicsDriver::Xe }; "xe timed out job with tile")]
#[test_case(&[XE_ENGINE_RESET], KernelFault::GraphicsReset { driver: GraphicsDriver::Xe }; "xe engine reset")]
#[test_case(&[I915_HANG], KernelFault::GraphicsTimeout { driver: GraphicsDriver::I915 }; "i915 hang")]
#[test_case(&[I915_RESET], KernelFault::GraphicsReset { driver: GraphicsDriver::I915 }; "i915 reset")]
#[test_case(&[XE_TIMEOUT], KernelFault::GraphicsTimeout { driver: GraphicsDriver::Xe }; "xe timed out job")]
#[test_case(&[XE_RESET], KernelFault::GraphicsReset { driver: GraphicsDriver::Xe }; "xe reset")]
#[test_case(&[OOPS], KernelFault::Oops {}; "oops")]
#[test_case(&[HUNG], KernelFault::HungTask {}; "hung task")]
#[test_case(&[UNCORRECTED], KernelFault::MachineCheck {}; "uncorrected machine check")]
#[test_case(&[FATAL], KernelFault::MachineCheck {}; "fatal machine check")]
fn each_recognized_line_is_one_fault_ending_the_log(lines: &[&str], fault: KernelFault) {
    assert_eq!(scan(&texts(lines)), only(fault, false));
}

#[test]
fn logging_after_the_last_error_is_recorded() {
    assert_eq!(scan(&texts(&[CALM, RING, CALM])), only(AMD_TIMEOUT, true));
    assert_eq!(
        scan(&texts(&[RING, CALM, RING])),
        only(AMD_TIMEOUT, false),
        "the last error decides, not the first"
    );
}

#[test]
fn faults_are_distinct_and_in_order_of_last_appearance() {
    assert_eq!(
        scan(&texts(&[RING, OOPS, RING, CALM])),
        KernelTail::Faults {
            faults: vec![
                KernelFault::Oops {},
                KernelFault::GraphicsTimeout {
                    driver: GraphicsDriver::Amdgpu
                }
            ],
            logging_continued: true,
        }
    );
}

#[test]
fn only_the_most_recent_distinct_faults_are_kept() {
    let mut lines: Vec<String> = (1..=u32::try_from(MAX_FAULTS).unwrap() + 4)
        .map(|code| format!("NVRM: Xid (PCI:0000:01:00): {code}, pid=1"))
        .collect();
    lines.push(CALM.to_owned());
    let KernelTail::Faults {
        faults,
        logging_continued,
    } = scan(&lines)
    else {
        panic!("Xids are faults");
    };
    assert!(logging_continued);
    assert_eq!(faults.len(), MAX_FAULTS);
    assert_eq!(faults[0], KernelFault::NvidiaXid { code: 5 });
    assert_eq!(faults[MAX_FAULTS - 1], KernelFault::NvidiaXid { code: 20 });
}

#[test]
fn no_entries_is_a_missing_tail() {
    assert_eq!(scan(&[]), KernelTail::Missing {});
}

#[test_case(&[CALM, CALM]; "calm lines")]
#[test_case(&[CORRECTED, "mce: [Hardware Error]: Machine check events logged"]; "corrected machine checks")]
#[test_case(&["NVRM: Xid (PCI:0000:01:00): not a number"]; "an Xid without a code")]
#[test_case(&["amdgpu 0000:03:00.0: amdgpu: SMU is initialized successfully!"]; "an amdgpu line about nothing wrong")]
#[test_case(&["a ring of timeout"]; "words alone are not a graphics driver")]
fn a_read_tail_without_errors_is_quiet(lines: &[&str]) {
    assert_eq!(scan(&texts(lines)), KernelTail::Quiet {});
}

const GRAPHICS_ROWS: [&str; 2] = [RING, "usb 1-2: reset"];

fn tail_rows(rows: &[&str]) -> Vec<u8> {
    kernel_lines(rows)
}

/// A journal that lists `[OLD, LATER]` with the boundary proving `LATER` follows `OLD`, and serves the tail
/// and the machine checks of each boot.
fn evidence_of(
    tail: Vec<u8>,
    old_mces: Vec<u8>,
    later_mces: Vec<u8>,
) -> (crate::Kernel, super::support::Calls) {
    kernel(&[(1, &[2, 18])], move |args| match args[0].as_str() {
        "--list-boots" => ok(boots(&[OLD, LATER])),
        "-b" if args[1] == OLD => ok(boundary(OLD, "series", "100")),
        "-b" => ok(boundary(LATER, "series", "101")),
        _ if args.iter().any(|arg| arg == "-n") => ok(&tail),
        _ if args[2] == OLD => ok(&old_mces),
        _ => ok(&later_mces),
    })
}

fn status(flags: &str) -> Vec<u8> {
    lines(&[serde_json::json!({
        "MESSAGE": format!("[Hardware Error]: CPU:2 (1a:44:0) MC0_STATUS[{flags}]: 0xbc00000000010135"),
        "__REALTIME_TIMESTAMP": "2000000",
        "__MONOTONIC_TIMESTAMP": "2000000",
    })])
}

#[test]
fn evidence_of_a_crashed_boot_joins_its_tail_and_both_boots_machine_checks() {
    let (kernel, calls) = evidence_of(
        tail_rows(&GRAPHICS_ROWS),
        status("Over|CE|-"),
        status("Over|UE|-"),
    );
    assert_eq!(
        kernel.end_evidence(&boot(OLD)),
        EndEvidence {
            crashed_mces: MceCount::Read {
                corrected: 1,
                uncorrected: 0
            },
            successor_mces: MceCount::Read {
                corrected: 0,
                uncorrected: 1
            },
            tail: only(
                KernelFault::GraphicsTimeout {
                    driver: GraphicsDriver::Amdgpu
                },
                true
            ),
            asleep: None,
        }
    );
    let tail_call = locked(&calls)
        .iter()
        .map(|call| call.args.clone())
        .find(|args| {
            args.first().is_some_and(|arg| arg == "-k") && args.iter().any(|arg| arg == "-n")
        })
        .unwrap();
    assert_eq!(
        tail_call.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "-k",
            "-b",
            OLD,
            "-o",
            "json",
            "--all",
            "--output-fields=MESSAGE",
            "-n",
            "256",
            "--no-pager",
            "-q",
        ]
    );
}

#[test]
fn a_successor_that_is_not_proven_leaves_its_machine_checks_unknown() {
    let (kernel, _) = kernel(&[(1, &[2, 18])], |args| match args[0].as_str() {
        "--list-boots" => ok(boots(&[OLD])),
        _ if args.iter().any(|arg| arg == "-n") => ok(kernel_lines(&[CALM])),
        _ => ok(b""),
    });
    assert_eq!(
        kernel.end_evidence(&boot(OLD)),
        EndEvidence {
            crashed_mces: MceCount::Read {
                corrected: 0,
                uncorrected: 0
            },
            successor_mces: MceCount::Unknown {},
            tail: KernelTail::Quiet {},
            asleep: None,
        }
    );
}

const SUSPEND_ENTRY: &str = "PM: suspend entry (s2idle)";
const SUSPEND_EXIT: &str = "PM: suspend exit";
const HIBERNATE_ENTRY: &str = "PM: hibernation: hibernation entry";
const HIBERNATE_EXIT: &str = "PM: hibernation: hibernation exit";

#[test_case(&[CALM], None; "no sleep in the tail")]
#[test_case(&[CALM, SUSPEND_ENTRY], Some(SleepMode::Suspend); "a suspend entry that ends the log")]
#[test_case(&["PM: suspend entry (deep)", "Freezing user space processes"], Some(SleepMode::Suspend); "deep suspend with later freezing lines")]
#[test_case(&[SUSPEND_ENTRY, SUSPEND_EXIT, CALM], None; "a suspend that exited")]
#[test_case(&[SUSPEND_ENTRY, SUSPEND_EXIT, SUSPEND_ENTRY], Some(SleepMode::Suspend); "the last entry decides")]
#[test_case(&[HIBERNATE_ENTRY, CALM], Some(SleepMode::Hibernate); "a hibernation entry")]
#[test_case(&[HIBERNATE_ENTRY, HIBERNATE_EXIT], None; "a hibernation that exited")]
#[test_case(&["usb 1-2: PM: suspend entry (s2idle)"], None; "text that only mentions suspend entry")]
fn the_tail_ends_asleep_only_after_an_entry_with_no_exit(rows: &[&str], want: Option<SleepMode>) {
    assert_eq!(sleep_end(&texts(rows)), want);
}

#[test]
fn evidence_names_the_sleep_the_crashed_boot_ended_in() {
    let (kernel, _) = kernel(&[(1, &[2, 18])], |args| match args[0].as_str() {
        "--list-boots" => ok(boots(&[OLD])),
        _ if args.iter().any(|arg| arg == "-n") => ok(kernel_lines(&[CALM, SUSPEND_ENTRY])),
        _ => ok(b""),
    });
    assert_eq!(
        kernel.end_evidence(&boot(OLD)).asleep,
        Some(SleepMode::Suspend)
    );
}

#[test]
fn a_boot_the_journal_no_longer_has_is_entirely_unknown() {
    let (kernel, _) = kernel(&[(1, &[2, 18])], |_| exit(1, b"", NO_BOOT));
    assert_eq!(kernel.end_evidence(&boot(OLD)), EndEvidence::unknown());
}

#[test]
fn an_invalid_boot_id_is_entirely_unknown() {
    let (kernel, calls) = kernel(&[], |_| ok(b""));
    assert_eq!(
        kernel.end_evidence(&BootId::from("not-a-boot".to_owned())),
        EndEvidence::unknown()
    );
    assert!(locked(&calls).is_empty());
}

#[test_case(exit(2, b"", "journal unavailable"); "journalctl fails")]
#[test_case(ok(b"{"); "undecodable output")]
#[test_case(ok(br#"{"MESSAGE":{}}"#); "an invalid message")]
fn a_tail_that_cannot_be_read_is_unreadable_never_quiet(
    reply: Result<togi_command::Output, RunError>,
) {
    let reply = std::sync::Mutex::new(Some(reply));
    let (kernel, _) = kernel(&[], move |args| {
        if args.iter().any(|arg| arg == "-n") {
            reply.lock().unwrap().take().expect("one tail query")
        } else {
            exit(1, b"", NO_BOOT)
        }
    });
    let evidence = kernel.end_evidence(&boot(OLD));
    assert_eq!(evidence.tail, KernelTail::Unreadable {});
    assert_eq!(evidence.crashed_mces, MceCount::Unknown {});
    assert!(!evidence.has_uncorrected_mce());
}

#[test]
fn argv_of_the_other_queries_is_unchanged_by_reading_evidence() {
    let (kernel, calls) = evidence_of(kernel_lines(&[CALM]), Vec::new(), Vec::new());
    let _ = kernel.end_evidence(&boot(OLD));
    let calls = locked(&calls);
    assert_eq!(
        argv(&calls[0]),
        ["--list-boots", "-o", "json", "--no-pager", "-q"]
    );
}
