//! `togi run` in process, against a simulated machine: the order of its steps, what refuses it before
//! hardware, and how each end of the session maps to a status.

use togi_cli::Exit;
use togi_hostlock::Policy;
use togi_journal::{EventKind, Payload, ShutdownReason};
use togi_machine::{CoreId, Machine, TargetOffset};
use togi_render::OutputFacts;

use crate::support::{Fake, biased, interrupt_at_first_trial, interrupt_on_first_write};

fn interrupted() -> Fake {
    let mut fake = Fake::new();
    fake.wrap = Box::new(interrupt_on_first_write);
    fake
}

fn seed_newer_epoch(fake: &mut Fake) {
    let wrap = std::mem::replace(&mut fake.wrap, Box::new(interrupt_on_first_write));
    let (exit, _, stderr) = fake.run(&["run", "--no-tui"]);
    fake.wrap = wrap;
    assert_eq!(exit, Exit::Ok, "{stderr}");
    let path = fake.state_dir().join("events.jsonl");
    let text = fake.read_to_string(&path).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    assert!(first.contains(r#""evidence":1"#), "{first}");
    fake.write(
        &path,
        format!(
            "{}\n{rest}",
            first.replace(r#""evidence":1"#, r#""evidence":2"#)
        ),
    )
    .unwrap();
}

#[test]
fn an_unsupported_platform_is_reported_before_anything_else_is_touched() {
    let mut fake = Fake::new();
    fake.supported = false;
    let (exit, out, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Error);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "togi run: hardware runs need Linux: unsupported operation\n"
    );
    assert_eq!(*fake.calls.borrow(), ["check_platform"]);
    assert!(!fake.exists(fake.state_dir()));
    assert!(!fake.exists(&fake.lock));
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
}

/// Every core's offset on the simulated machine, read back through its SMU.
fn offsets(fake: &Fake) -> Vec<i64> {
    let mut smu = fake.sim.seams().smu;
    (0..16)
        .map(|core| smu.offset(CoreId::new(core).unwrap()).unwrap().0)
        .collect()
}

/// A run stopped by a signal restored what it had written (`profile.restored` before the final shutdown), and
/// the machine reads back at its boot offsets.
fn assert_stopped_by_a_signal_with_the_offsets_restored(fake: &Fake, boot: i64, what: &str) {
    let events = fake.events();
    let last = events.last().unwrap();
    assert!(
        matches!(&last.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal),
        "{what}: {}",
        last.kind().as_str()
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.payload, Payload::SmuIntent(_))),
        "{what}: the run wrote nothing, so restoring it proves nothing"
    );
    let restored = events
        .iter()
        .rposition(|event| event.kind() == EventKind::ProfileRestored)
        .unwrap_or_else(|| panic!("{what}: no profile.restored"));
    assert!(
        restored + 1 < events.len(),
        "{what}: restored after the shutdown"
    );
    assert_eq!(offsets(fake), vec![boot; 16], "{what}: not restored");
}

#[test]
fn cancellation_stops_a_run_cleanly_with_the_offsets_restored_and_the_shutdown_recorded() {
    let mut fake = Fake::on(biased(1000, -5));
    fake.wrap = Box::new(interrupt_on_first_write);
    let (exit, out, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("session.start"), "{err}");
    assert_stopped_by_a_signal_with_the_offsets_restored(&fake, -5, "SIGINT");
    assert_eq!(fake.opened.get(), 1);
}

#[test]
fn a_journal_with_a_newer_evidence_epoch_is_refused_before_the_config_is_read() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    let path = fake.state_dir().join("events.jsonl");
    let before = fake.read(&path).unwrap();
    let opened = fake.opened.get();

    let missing = fake.dir.join("missing.json");
    let (exit, _, err) = fake.run(&["run", "--config", missing.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(err.contains("evidence epoch"), "{err}");
    assert_eq!(fake.read(&path).unwrap(), before);
    assert_eq!(fake.opened.get(), opened);
}

#[test]
fn a_refused_journal_in_the_tuning_boot_leaves_a_reason_and_clears_the_saved_entry() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(
        err.contains("cleared GRUB saved entry from \"tuning\""),
        "{err}"
    );
    let state = fake.grub.state();
    assert_eq!(state.cleared, 1);
    assert!(
        state.values.contains_key("togi_leave_reason"),
        "{:?}",
        state.values
    );
    assert_eq!(fake.reboots.get(), 0);
}

#[test]
fn a_refused_journal_without_the_tuning_boot_leaves_the_boot_entry_alone() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let (exit, _, _) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Incompatible);
    assert_eq!(fake.grub.state().cleared, 0);
}

#[test]
fn a_tuning_boot_refusal_does_not_touch_the_boot_entry_without_the_host_lock() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let held = fake.hold_lock();
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    drop(held);
    assert_eq!(exit, Exit::Locked, "{err}");
    let state = fake.grub.state();
    assert_eq!(state.cleared, 0);
    assert!(
        state.log.is_empty(),
        "neither a leave reason nor a clear may be written: {:?}",
        state.log
    );
    assert_eq!(
        state.values.keys().collect::<Vec<_>>(),
        ["saved_entry"],
        "{:?}",
        state.values
    );
}

#[test]
fn a_tuning_boot_refusal_does_not_touch_the_boot_entry_when_the_host_lock_is_not_safe() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    fake.policy = Policy::Host;
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert!(err.starts_with("togi run: host lock"), "{err}");
    let state = fake.grub.state();
    assert_eq!(state.cleared, 0);
    assert!(state.log.is_empty(), "{:?}", state.log);
}

#[test]
fn a_refusal_under_journald_is_printed_at_err_priority_in_bold_red() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.facts = OutputFacts {
        journald: true,
        ..OutputFacts::default()
    };
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(
        err.starts_with("<3>\x1b[1;31mtogi run: "),
        "the refusal lost its journald priority or its style: {err:?}"
    );
}

#[test]
fn the_host_lock_is_taken_before_the_journals_lock_as_reset_does() {
    let fake = Fake::new();
    let held = fake.hold_lock();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    drop(held);
    assert_eq!(exit, Exit::Locked, "{err}");
    assert!(
        !fake.exists(fake.state_dir()),
        "the journal's lock was taken while the host lock was busy"
    );

    let fake = Fake::new();
    let held = togi_cli::system::System::lock_state_dir(&fake, &fake.state_dir()).unwrap();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    drop(held);
    assert_eq!(exit, Exit::Locked, "{err}");
    assert_eq!(
        *fake.calls.borrow(),
        ["check_platform", "host_lock"],
        "the host lock was not taken before the journal's"
    );
    assert_eq!(fake.opened.get(), 0);
}

#[test]
fn an_invalid_config_is_a_usage_error_for_a_compatible_journal() {
    let fake = Fake::new();
    let config = fake.dir.join("config.json");
    fake.write(&config, r#"{"bogus": 1}"#).unwrap();
    let (exit, _, err) = fake.run(&["run", "--config", config.to_str().unwrap()]);
    assert_eq!(exit, Exit::Usage, "{err}");
    assert!(err.starts_with("togi run: invalid config"), "{err}");
    assert_eq!(fake.opened.get(), 0);
}

#[test]
fn a_missing_explicit_config_is_a_usage_error() {
    let fake = Fake::new();
    let missing = fake.dir.join("missing.json");
    let (exit, _, err) = fake.run(&["run", "--config", missing.to_str().unwrap()]);
    assert_eq!(exit, Exit::Usage, "{err}");
    assert!(err.contains("cannot read config"), "{err}");
}

/// The configuration `config.loaded` recorded: the path, and whether a file supplied it.
fn recorded_config(fake: &Fake) -> (String, bool) {
    fake.events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::ConfigLoaded(loaded) => Some((loaded.path.clone(), loaded.file)),
            _ => None,
        })
        .unwrap()
}

#[test]
fn a_missing_default_config_is_the_default_configuration_and_is_recorded_as_no_file() {
    let fake = interrupted();
    let (exit, err) = fake.run_on_default_config(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    assert_eq!(
        recorded_config(&fake),
        (fake.default_config.display().to_string(), false)
    );
}

#[test]
fn a_default_config_that_exists_is_read_and_recorded_as_a_file() {
    let fake = interrupted();
    fake.write(&fake.default_config, "{}").unwrap();
    let (exit, err) = fake.run_on_default_config(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    assert_eq!(
        recorded_config(&fake),
        (fake.default_config.display().to_string(), true)
    );
}

#[test]
fn the_config_flag_may_precede_the_command_name() {
    let fake = Fake::new();
    let config = fake.dir.join("config.json");
    fake.write(&config, r#"{"bogus": 1}"#).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = togi_cli::run_with(
        [
            "togi",
            "--config",
            config.to_str().unwrap(),
            "--state-dir",
            fake.state_dir().to_str().unwrap(),
            "run",
        ],
        &mut out,
        &mut err,
        &fake,
    );
    assert_eq!(exit, Exit::Usage);
    assert!(String::from_utf8(err).unwrap().contains("invalid config"));
}

#[test]
fn a_busy_host_lock_stops_the_run_before_hardware_and_any_event() {
    let fake = Fake::new();
    let held = fake.hold_lock();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    drop(held);
    assert_eq!(exit, Exit::Locked, "{err}");
    assert!(err.contains("host lock"), "{err}");
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
    assert!(fake.events().is_empty());
}

#[test]
fn a_busy_journal_lock_is_exit_three() {
    let fake = Fake::new();
    let held = togi_cli::system::System::lock_state_dir(&fake, &fake.state_dir()).unwrap();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    drop(held);
    assert_eq!(exit, Exit::Locked, "{err}");
    assert_eq!(fake.opened.get(), 0);
}

#[test]
fn a_journal_lock_failure_prints_its_path_and_cause_once() {
    let mut fake = Fake::new();
    let fs = togi_journal::MemFs::new();
    fs.put(fake.dir.join("config.json"), b"{}".to_vec());
    fake.fs = std::sync::Arc::new(fs.clone());
    fs.inject(togi_journal::fs::Fault::before(togi_journal::fs::Op::Open, 1).named("lock"));
    let (exit, out, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "togi run: lock /run-test/state/lock: injected Open fault\n"
    );
    assert_eq!(fake.opened.get(), 0);
}

#[test]
fn a_locked_journal_read_failure_prints_its_path_and_cause_once() {
    let mut fake = Fake::new();
    let fs = togi_journal::MemFs::new();
    fs.put(fake.dir.join("config.json"), b"{}".to_vec());
    fake.fs = std::sync::Arc::new(fs.clone());
    *fake.at_host_lock.borrow_mut() = Some(Box::new(move || {
        fs.inject(
            togi_journal::fs::Fault::before(togi_journal::fs::Op::Read, 1).named("events.jsonl"),
        );
    }));
    let (exit, out, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "togi run: read journal /run-test/state/events.jsonl: injected Read fault\n"
    );
    assert_eq!(fake.opened.get(), 0);
}

#[test]
fn a_journal_close_failure_keeps_its_cause_and_changes_the_exit_status() {
    use togi_journal::fs::{Fault, Op};

    let on_memory = || {
        let mut fake = interrupted();
        let fs = togi_journal::MemFs::new();
        fs.put(fake.dir.join("config.json"), b"{}".to_vec());
        fake.fs = std::sync::Arc::new(fs.clone());
        (fake, fs)
    };
    let (baseline, fs) = on_memory();
    fs.record_history();
    let (exit, _, baseline_err) = baseline.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Ok);
    let syncs = fs
        .log()
        .iter()
        .filter(|(op, path)| {
            *op == Op::Sync && path.file_name().is_some_and(|name| name == "events.jsonl")
        })
        .count();
    assert!(syncs > 0);

    let (fake, fs) = on_memory();
    fs.inject(Fault::before(Op::Sync, u64::try_from(syncs).unwrap()).named("events.jsonl"));
    let (exit, out, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error);
    assert_eq!(out, "");
    assert_eq!(
        err,
        format!("togi run: close journal: injected Sync fault\n{baseline_err}")
    );
}

#[test]
fn a_failed_smu_write_is_the_smu_dead_end_and_exits_eleven() {
    let fake = Fake::new();
    fake.sim.fail_write(1);
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Smu, "{err}");
    assert!(err.contains("togi: dead end smu:"), "{err}");
    assert!(
        fake.events()
            .iter()
            .any(|event| event.kind() == EventKind::DeadEnd)
    );
}

/// `text` with an event of a kind this build does not know at its end.
fn with_a_future_kind(text: &str) -> String {
    let next = text.lines().count() + 1;
    format!(
        "{text}{}\n",
        r#"{"seq":N,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"future.thing","msg":"x","data":{}}"#
            .replace('N', &next.to_string())
    )
}

fn append_future_kind(fake: &Fake) -> Vec<u8> {
    let path = fake.state_dir().join("events.jsonl");
    let text = with_a_future_kind(&fake.read_to_string(&path).unwrap());
    fake.write(&path, &text).unwrap();
    text.into_bytes()
}

/// Has the journal replaced by `text` when the run asks for the host lock: after its first read of the journal,
/// which holds no lock, and before its second, under both locks. A refusal the first read missed can only come
/// from the second.
fn replace_the_journal_at_the_host_lock(fake: &Fake, text: String) {
    let path = fake.state_dir().join("events.jsonl");
    let fs = std::sync::Arc::clone(&fake.fs);
    *fake.at_host_lock.borrow_mut() = Some(Box::new(move || {
        fs.create_truncate(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }));
}

fn first_run(fake: &mut Fake) {
    let wrap = std::mem::replace(&mut fake.wrap, Box::new(interrupt_on_first_write));
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    fake.wrap = wrap;
    assert_eq!(exit, Exit::Ok, "{err}");
}

#[test]
fn an_unknown_event_kind_in_a_current_journal_is_refused_before_the_config_naming_the_kind() {
    let mut fake = Fake::new();
    first_run(&mut fake);
    let before = append_future_kind(&fake);
    let missing = fake.dir.join("missing.json");
    let (exit, _, err) = fake.run(&["run", "--config", missing.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(err.contains(r#"unknown kind "future.thing""#), "{err}");
    assert!(
        err.contains("cannot safely modify it; install the build that wrote it"),
        "{err}"
    );
    assert_eq!(
        fake.read(fake.state_dir().join("events.jsonl")).unwrap(),
        before
    );
}

#[test]
fn an_unknown_event_field_in_a_current_journal_is_refused_naming_its_path_and_writes_nothing() {
    let mut fake = Fake::new();
    first_run(&mut fake);
    let path = fake.state_dir().join("events.jsonl");
    let text = fake.read_to_string(&path).unwrap();
    let next = text.lines().count() + 1;
    let before = format!(
        r#"{text}{{"seq":{next},"time":"2026-01-01T00:00:00Z","boot":"b","kind":"session.warning","msg":"x","data":{{"operation":"x","error":"y","future_field":1}}}}"#
    ) + "\n";
    fake.write(&path, &before).unwrap();
    let missing = fake.dir.join("missing.json");
    let (exit, _, err) = fake.run(&["run", "--config", missing.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(
        err.contains(r#"unknown field "data.future_field" of a session.warning event"#),
        "{err}"
    );
    assert!(
        err.contains("cannot safely modify it; install the build that wrote it"),
        "{err}"
    );
    assert_eq!(fake.read(&path).unwrap(), before.into_bytes());
}

#[test]
fn an_unknown_event_kind_in_the_tuning_boot_leaves_its_own_reason_and_clears_the_saved_entry() {
    let mut fake = Fake::new();
    first_run(&mut fake);
    append_future_kind(&fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    let state = fake.grub.state();
    assert_eq!(state.cleared, 1);
    assert!(
        state
            .values
            .get("togi_leave_reason")
            .is_some_and(|reason| reason.contains("unknown event kind")),
        "{:?}",
        state.values
    );
}

/// What the locked read of a journal changed after the first read must do: refuse with status 16 naming
/// `reason`, before the boot id is read or the hardware opened, leave the journal exactly as `changed`, and in the
/// tuning boot leave `leave` as the reason and clear the saved entry, without a reboot.
fn assert_refused_under_the_locks(
    fake: &Fake,
    changed: &str,
    (exit, err): (Exit, &str),
    reason: &str,
    leave: &str,
    before: (u32, u32, usize),
) {
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(err.contains(reason), "{err}");
    assert_eq!(
        fake.read(fake.state_dir().join("events.jsonl")).unwrap(),
        changed.as_bytes(),
        "the refused journal is left as it was found"
    );
    let (opened, boots, calls) = before;
    assert_eq!(
        (fake.opened.get(), fake.boots_read.get()),
        (opened, boots),
        "neither the boot id nor the hardware is touched"
    );
    assert_eq!(
        fake.calls.borrow()[calls..],
        ["check_platform", "host_lock"],
        "the first read passed, so only the locked one can have refused"
    );
    let state = fake.grub.state();
    assert_eq!(state.cleared, 1, "{:?}", state.values);
    assert!(
        state
            .values
            .get("togi_leave_reason")
            .is_some_and(|text| text.contains(leave)),
        "{:?}",
        state.values
    );
    assert_eq!(fake.reboots.get(), 0);
}

#[test]
fn a_journal_that_became_newer_after_the_first_read_is_refused_under_the_locks() {
    let mut fake = Fake::new();
    first_run(&mut fake);
    let path = fake.state_dir().join("events.jsonl");
    let text = fake.read_to_string(&path).unwrap();
    let newer = text.replacen(r#""evidence":1"#, r#""evidence":2"#, 1);
    assert_ne!(newer, text);
    replace_the_journal_at_the_host_lock(&fake, newer.clone());
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let grubenv = fake.grubenv();
    let before = (
        fake.opened.get(),
        fake.boots_read.get(),
        fake.calls.borrow().len(),
    );
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_refused_under_the_locks(
        &fake,
        &newer,
        (exit, &err),
        "evidence epoch",
        "journal incompatible",
        before,
    );
}

#[test]
fn a_journal_that_gained_an_unknown_kind_after_the_first_read_is_refused_under_the_locks() {
    let mut fake = Fake::new();
    first_run(&mut fake);
    let path = fake.state_dir().join("events.jsonl");
    let changed = with_a_future_kind(&fake.read_to_string(&path).unwrap());
    replace_the_journal_at_the_host_lock(&fake, changed.clone());
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let grubenv = fake.grubenv();
    let before = (
        fake.opened.get(),
        fake.boots_read.get(),
        fake.calls.borrow().len(),
    );
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_refused_under_the_locks(
        &fake,
        &changed,
        (exit, &err),
        r#"unknown kind "future.thing""#,
        "unknown event kind",
        before,
    );
}

#[test]
fn a_journal_that_is_not_a_journal_is_an_error_and_is_left_alone() {
    let fake = Fake::new();
    fake.create_dir(fake.state_dir()).unwrap();
    let path = fake.state_dir().join("events.jsonl");
    fake.write(&path, "not json\n").unwrap();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert!(err.starts_with("togi run: "), "{err}");
    assert_eq!(fake.read_to_string(&path).unwrap(), "not json\n");
    assert_eq!(fake.opened.get(), 0);
}

#[test]
fn the_refusal_persists_its_reason_before_it_clears_the_entry() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let grubenv = fake.grubenv();
    let (exit, _, _) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible);
    let state = fake.grub.state();
    assert_eq!(state.log, ["set", "clear"]);
    assert!(
        state.values["togi_leave_reason"].contains("journal incompatible"),
        "{:?}",
        state.values
    );
}

#[test]
fn the_refusal_still_clears_the_entry_when_the_reason_cannot_be_written() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    fake.grub.state().fail_set = true;
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(err.contains("togi: persist GRUB leave reason:"), "{err}");
    assert_eq!(fake.grub.state().cleared, 1);
}

#[test]
fn the_refusal_reports_a_failed_clear_and_requests_no_reboot() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    fake.grub.state().fail_clear = true;
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(
        err.contains("togi: clear GRUB saved entry:") && err.contains("no reboot requested"),
        "{err}"
    );
    assert_eq!(fake.reboots.get(), 0);
}

#[test]
fn a_hardware_that_cannot_be_opened_is_reported_as_an_error() {
    let mut fake = Fake::new();
    fake.open_error = true;
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error);
    assert!(err.starts_with("togi run: open the SMU driver"), "{err}");
    assert!(fake.events().is_empty());
}

#[test]
fn a_host_lock_that_is_not_safe_is_refused_before_hardware() {
    let mut fake = Fake::new();
    fake.policy = Policy::Host;
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert!(err.starts_with("togi run: host lock"), "{err}");
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
    assert!(fake.events().is_empty());
}

#[test]
fn a_host_lock_that_cannot_be_taken_names_its_cause_once_before_hardware() {
    let fake = Fake::new();
    *fake.lock_error.borrow_mut() = Some(togi_hostlock::Error::Io {
        path: fake.lock.clone(),
        operation: togi_hostlock::Operation::Lock,
        source: std::io::Error::other("disk gone"),
    });
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert_eq!(
        err,
        format!(
            "togi run: lock host lock {}: disk gone\n",
            fake.lock.display()
        )
    );
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
    assert!(fake.events().is_empty());
}

#[test]
fn a_signal_listener_that_does_not_start_names_the_failed_step_before_hardware() {
    let fake = Fake::new();
    *fake.listen_error.borrow_mut() = Some(togi_cli::signals::Error::Start(std::sync::Arc::new(
        togi_cli::signals::StartError::Register(std::io::Error::other("no handlers left")),
    )));
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert_eq!(
        err,
        "togi run: start the signal listener: register the signal handlers: no handlers left\n"
    );
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
    assert!(fake.events().is_empty());
}

#[test]
fn the_bios_context_is_not_read_from_a_machine_whose_identity_is_not_validated() {
    let bios_reads = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut fake = Fake::new();
    let seen = bios_reads.clone();
    fake.wrap = Box::new(move |machine, _| Machine {
        host: Box::new(crate::support::UnvalidatedHost {
            inner: machine.host,
            bios_reads: seen.clone(),
        }),
        ..machine
    });
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Preflight, "{err}");
    assert!(err.contains("unsupported CPU model"), "{err}");
    assert_eq!(bios_reads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn the_dashboard_hides_event_lines_and_the_clean_stop_prints_the_restoration_and_shutdown() {
    let mut fake = Fake::new();
    fake.interactive = true;
    fake.wrap = Box::new(interrupt_on_first_write);
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    assert!(!err.contains("session.start"), "{err}");
    assert!(!err.contains("trial.intent"), "{err}");
    assert!(err.trim_end().ends_with("stopped by signal"), "{err}");
    assert!(err.contains(" shutdown "), "{err}");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the fixture seeds applied simulated offsets before resume"
)]
fn a_restoration_the_dashboard_hid_at_a_resumes_startup_is_not_repeated_by_the_clean_stop() {
    // A run in the same boot left offsets applied, so the resume restores them at its startup, while the
    // dashboard shows, and then goes on to work: only the restoration and the shutdown of its own stop belong
    // on stderr once it ends.
    let mut first = Fake::on(biased(1000, -5));
    first_run(&mut first);
    first
        .sim
        .seams()
        .smu
        .set_all_offsets(TargetOffset::new(-20).unwrap())
        .unwrap();
    let earlier = first.events().len();

    let mut fake = first.resume();
    fake.interactive = true;
    fake.wrap = Box::new(interrupt_at_first_trial);
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Ok, "{err}");

    let events = fake.events();
    let resumed = &events[earlier..];
    let restored: Vec<usize> = resumed
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind() == EventKind::ProfileRestored)
        .map(|(index, _)| index)
        .collect();
    let [startup, closing] = restored[..] else {
        panic!("the resume restored {} times: {restored:?}", restored.len());
    };
    assert!(
        resumed[startup + 1..closing]
            .iter()
            .any(|event| event.kind() != EventKind::SessionWarning),
        "work followed the startup restoration, so it is not the closing one"
    );
    assert_eq!(err.matches(" profile.restored ").count(), 1, "{err}");
    assert_eq!(err.matches(" shutdown ").count(), 1, "{err}");
    assert!(
        err.find(" profile.restored ") < err.find(" shutdown "),
        "{err}"
    );
    assert!(!err.contains("trial.intent"), "{err}");
}

#[test]
fn the_dashboard_starts_after_the_last_event_of_the_earlier_run() {
    let mut first = Fake::new();
    first_run(&mut first);
    let earlier = first.events().len() as u64;

    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut fake = first.resume();
    fake.interactive = true;
    fake.wrap = Box::new(interrupt_on_first_write);
    fake.surface = std::sync::Arc::new(Records {
        seen: seen.clone(),
        fs: fake.fs.clone(),
    });
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    let (since, _) = seen.lock().unwrap().unwrap();
    assert_eq!(
        since, earlier,
        "it must say STARTING until an event after the earlier run's last"
    );
}

#[test]
fn the_dashboard_starts_after_the_journal_is_open() {
    // A fresh state directory holds no `events.jsonl` until the run's `prepare` opens it.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut fake = Fake::new();
    fake.interactive = true;
    fake.wrap = Box::new(interrupt_on_first_write);
    fake.surface = std::sync::Arc::new(Records {
        seen: seen.clone(),
        fs: fake.fs.clone(),
    });
    assert!(!fake.exists(fake.state_dir().join("events.jsonl")));
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    let (since, journal_existed) = seen.lock().unwrap().unwrap();
    assert_eq!(since, 0);
    assert!(
        journal_existed,
        "the dashboard started before the journal was open"
    );
}

struct Records {
    seen: std::sync::Arc<std::sync::Mutex<Option<(u64, bool)>>>,
    fs: std::sync::Arc<dyn togi_journal::fs::Fs>,
}

impl togi_cli::dashboard::Surface for Records {
    fn draw(
        &self,
        state_dir: &std::path::Path,
        since: u64,
        stop: &togi_cli::dashboard::Stop,
    ) -> Result<(), togi_watch::terminal::Error> {
        *self.seen.lock().unwrap() = Some((
            since,
            self.fs
                .kind(&state_dir.join("events.jsonl"))
                .unwrap()
                .is_some(),
        ));
        stop.wait();
        Ok(())
    }
}

#[test]
fn real_process_sighup_and_sigterm_stop_a_run_cleanly_like_sigint() {
    for signal in [rustix::process::Signal::HUP, rustix::process::Signal::TERM] {
        crate::pty::stopped_by_signal(signal);
    }
}

fn position(events: &[togi_journal::Event], wanted: impl Fn(&Payload) -> bool) -> Option<usize> {
    events.iter().position(|event| wanted(&event.payload))
}

#[test]
fn a_missing_watchdog_is_journaled_as_a_warning_before_the_first_offset_write_with_the_dashboard_up()
 {
    let mut fake = Fake::new();
    fake.interactive = true;
    fake.sim.fail_check("watchdog", "no /dev/watchdog");
    fake.wrap = Box::new(interrupt_on_first_write);
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    let events = fake.events();
    let warned = position(&events, |payload| {
        matches!(payload, Payload::SessionWarning(warning) if warning.operation == "check hardware watchdog"
            && warning.error.starts_with("no active hardware watchdog (no /dev/watchdog)"))
    })
    .unwrap();
    let first_write =
        position(&events, |payload| matches!(payload, Payload::SmuIntent(_))).unwrap();
    assert!(warned < first_write, "{warned} before {first_write}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(&event.payload, Payload::PreflightCheck(check) if check.check == "watchdog")),
        "a manual run does not wait for the watchdog"
    );
}

#[test]
fn the_tuning_boot_waits_for_the_watchdog_and_a_manual_run_does_not() {
    let manual = {
        let mut fake = Fake::new();
        fake.sim.fail_check("watchdog", "no /dev/watchdog");
        fake.wrap = Box::new(interrupt_on_first_write);
        let (exit, _, err) = fake.run(&["run", "--no-tui"]);
        assert_eq!(exit, Exit::Ok, "{err}");
        assert!(
            !fake
                .events()
                .iter()
                .any(|event| matches!(&event.payload, Payload::PreflightCheck(check) if check.check == "watchdog")),
            "a manual run does not wait for a watchdog handoff"
        );
        fake
    };
    assert!(
        manual
            .events()
            .iter()
            .any(|event| matches!(&event.payload, Payload::SmuIntent(_)))
    );

    let mut boot = Fake::new();
    boot.sim.fail_check("watchdog", "no /dev/watchdog");
    boot.wrap = Box::new(interrupt_on_first_write);
    let before = boot.sim.monotonic();
    let grubenv = boot.grubenv();
    let (exit, _, err) = boot.run(&[
        "run",
        "--no-tui",
        "--tuning-boot",
        grubenv.to_str().unwrap(),
    ]);
    assert_eq!(exit, Exit::Preflight, "{err}");
    assert!(boot.sim.monotonic().saturating_sub(before) >= std::time::Duration::from_secs(30));
    assert!(
        !boot
            .events()
            .iter()
            .any(|event| matches!(&event.payload, Payload::SmuIntent(_))),
        "no offset is written before the watchdog is armed"
    );
}

#[test]
fn the_host_lock_is_taken_before_the_boot_id_is_read_and_the_hardware_opened() {
    let fake = interrupted();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    assert_eq!(
        *fake.calls.borrow(),
        ["check_platform", "host_lock", "boot_id", "open_machine"]
    );
}

#[test]
fn an_explicit_config_that_is_missing_is_a_usage_error_wherever_the_flag_is_written() {
    let fake = Fake::new();
    let missing = fake.dir.join("missing.json");
    let missing = missing.to_str().unwrap();
    let state = fake.state_dir();
    for args in [
        [
            "togi",
            "--config",
            missing,
            "--state-dir",
            state.to_str().unwrap(),
            "run",
        ],
        [
            "togi",
            "--state-dir",
            state.to_str().unwrap(),
            "run",
            "--config",
            missing,
        ],
    ] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = togi_cli::run_with(args, &mut out, &mut err, &fake);
        let err = String::from_utf8(err).unwrap();
        assert_eq!(exit, Exit::Usage, "{args:?}: {err}");
        assert!(err.contains("cannot read config"), "{err}");
    }
    assert_eq!(fake.opened.get(), 0);
    assert!(fake.events().is_empty());
}

#[test]
fn an_unknown_kind_in_an_older_journal_does_not_refuse_the_run_before_the_config_is_read() {
    for field in ["ruleset", "schema", "evidence"] {
        let mut fake = Fake::new();
        first_run(&mut fake);
        let path = fake.state_dir().join("events.jsonl");
        let text = fake.read_to_string(&path).unwrap();
        let (first, rest) = text.split_once('\n').unwrap();
        let marker = format!(r#""{field}":"#);
        let at = first
            .find(&marker)
            .unwrap_or_else(|| panic!("{field} in {first}"))
            + marker.len();
        let digits = first[at..].bytes().take_while(u8::is_ascii_digit).count();
        let value: u32 = first[at..at + digits].parse().unwrap();
        let older = value
            .checked_sub(1)
            .unwrap_or_else(|| panic!("{field} {value} has no older stamp"));
        fake.write(
            &path,
            format!("{}{older}{}\n{rest}", &first[..at], &first[at + digits..]),
        )
        .unwrap();
        let before = append_future_kind(&fake);
        let missing = fake.dir.join("missing.json");
        let (exit, _, err) = fake.run(&["run", "--config", missing.to_str().unwrap()]);
        assert_eq!(exit, Exit::Usage, "older {field}: {err}");
        assert!(err.contains("cannot read config"), "older {field}: {err}");
        assert!(!err.contains("unknown kind"), "older {field}: {err}");
        assert_eq!(fake.read(&path).unwrap(), before, "older {field}");
    }
}

struct Finishes(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl togi_cli::dashboard::Surface for Finishes {
    fn draw(
        &self,
        _: &std::path::Path,
        _: u64,
        stop: &togi_cli::dashboard::Stop,
    ) -> Result<(), togi_watch::terminal::Error> {
        stop.wait();
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

struct NotesFirstWrite {
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    seen: Option<bool>,
}

impl std::io::Write for NotesFirstWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.seen
            .get_or_insert_with(|| self.finished.load(std::sync::atomic::Ordering::SeqCst));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failure_after_the_dashboard_showed_stops_and_joins_it_before_the_diagnostic_prints() {
    let mut fake = Fake::new();
    fake.interactive = true;
    fake.sim.fail_write(1);
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    fake.surface = std::sync::Arc::new(Finishes(finished.clone()));
    let mut err = NotesFirstWrite {
        finished,
        seen: None,
    };
    let mut out = Vec::new();
    let exit = togi_cli::run_with(fake.arguments(&["run"]), &mut out, &mut err, &fake);
    assert_eq!(exit, Exit::Smu);
    assert_eq!(
        err.seen,
        Some(true),
        "the diagnostic printed over a live dashboard"
    );
}

#[test]
fn a_journal_that_is_not_a_journal_never_shows_the_dashboard() {
    let mut fake = Fake::new();
    fake.interactive = true;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    fake.surface = std::sync::Arc::new(Records {
        seen: seen.clone(),
        fs: fake.fs.clone(),
    });
    fake.create_dir(fake.state_dir()).unwrap();
    fake.write(fake.state_dir().join("events.jsonl"), "not json\n")
        .unwrap();
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert!(seen.lock().unwrap().is_none(), "the dashboard drew");
}

#[test]
fn a_journal_that_cannot_be_opened_never_shows_the_dashboard() {
    // The state directory's `archive` is a file, so `prepare` fails after the hardware is open and before any
    // journal exists; with the dashboard moved before `prepare` it would have drawn by then.
    let mut fake = Fake::new();
    fake.interactive = true;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    fake.surface = std::sync::Arc::new(Records {
        seen: seen.clone(),
        fs: fake.fs.clone(),
    });
    fake.create_dir(fake.state_dir()).unwrap();
    fake.write(fake.state_dir().join("archive"), "not a directory")
        .unwrap();
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert_eq!(
        (fake.opened.get(), fake.events().len()),
        (1, 0),
        "it must fail in prepare, after the hardware was opened: {err}"
    );
    assert!(seen.lock().unwrap().is_none(), "the dashboard drew");
}

/// Leaves `fake`'s state directory with a journal of an earlier boot that ended in a boot-loop dead end whose
/// action clears the saved entry and reboots, not yet carried out, and puts the machine on a new boot.
fn seed_a_pending_boot_loop_dead_end(fake: &mut Fake) {
    fake.sim.fail_write(1);
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Smu, "{err}");
    let path = fake.state_dir().join("events.jsonl");
    let text = fake.read_to_string(&path).unwrap();
    let before: Vec<&str> = text
        .lines()
        .take_while(|line| !line.contains(r#""kind":"trial.intent""#))
        .collect();
    let field = |line: &str, name: &str| -> String {
        let at = line.find(name).unwrap() + name.len();
        line[at..]
            .chars()
            .take_while(|c| *c != '"' && *c != ',' && *c != '}')
            .collect()
    };
    let boot = field(before[0], r#""boot":""#);
    let baseline = before
        .iter()
        .find(|line| line.contains(r#""kind":"session.baseline""#))
        .unwrap();
    let cause = field(baseline, r#""seq":"#);
    let next = before.len() + 1;
    let dead_end = format!(
        r#"{{"seq":{next},"time":"2026-01-01T00:00:00Z","boot":"{boot}","mono_ms":0,"cause":[{cause}],"kind":"deadend","msg":"dead end boot_loop: three crashes before application","data":{{"condition":"boot_loop","detail":"three crashes before application","action":"clear_saved_entry_and_reboot"}}}}"#
    );
    fake.write(&path, format!("{}\n{dead_end}\n", before.join("\n")))
        .unwrap();
    fake.sim.reboot();
}

#[test]
fn a_tuning_boot_dead_end_that_asks_for_a_reboot_closes_the_journal_and_releases_the_lock_before_it_reboots()
 {
    let mut fake = Fake::new();
    seed_a_pending_boot_loop_dead_end(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&[
        "run",
        "--no-tui",
        "--tuning-boot",
        grubenv.to_str().unwrap(),
    ]);
    assert_eq!(exit, Exit::BootLoop, "{err}");
    assert!(err.contains("togi: dead end boot_loop:"), "{err}");
    assert!(
        err.find("togi: dead end boot_loop:") < err.find("togi: rebooting into the normal system"),
        "{err}"
    );
    assert!(
        err.contains("togi: rebooting into the normal system"),
        "{err}"
    );
    assert_eq!(fake.reboots.get(), 1);
    assert_eq!(
        fake.free_at_reboot.get(),
        Some((true, true)),
        "the host lock and the journal's lock must be free when the reboot is requested"
    );
    assert_eq!(fake.grub.state().cleared, 1);
    assert_eq!(
        fake.events().last().map(togi_journal::Event::kind),
        Some(EventKind::Shutdown)
    );
}

#[test]
fn a_tuning_boot_dead_end_whose_reboot_fails_reports_it_and_keeps_the_dead_end_status() {
    let mut fake = Fake::new();
    seed_a_pending_boot_loop_dead_end(&mut fake);
    fake.grub = crate::support::Grub::with_saved_entry("tuning");
    fake.fail_reboot = true;
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&[
        "run",
        "--no-tui",
        "--tuning-boot",
        grubenv.to_str().unwrap(),
    ]);
    assert_eq!(exit, Exit::BootLoop, "{err}");
    assert!(
        err.contains(
            "togi: systemctl reboot: systemctl reboot exited with exit status: 1: Failed to reboot"
        ),
        "{err}"
    );
    assert_eq!(fake.reboots.get(), 1);
}

#[test]
fn a_dead_end_that_asks_for_no_reboot_does_not_reboot() {
    let mut fake = Fake::new();
    seed_a_pending_boot_loop_dead_end(&mut fake);
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::BootLoop, "{err}");
    assert_eq!(fake.reboots.get(), 0, "{err}");
}
