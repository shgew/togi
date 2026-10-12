//! `togi run` in process, against a simulated machine: the order of its steps, what refuses it before
//! hardware, and how each end of the session maps to a status. The Go tests these replace are listed in
//! `docs/rust-migration.md`.

use std::fs;

use togi_cli::Exit;
use togi_hostlock::{HostLock, Policy};
use togi_journal::{EventKind, Payload, ShutdownReason, StateDir};

use crate::support::{Fake, interrupt_on_first_write};

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
    let text = fs::read_to_string(&path).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    assert!(first.contains(r#""evidence":1"#), "{first}");
    fs::write(
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
    assert!(!fake.state_dir().exists());
    assert!(!fake.lock.exists());
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
}

#[test]
fn sigint_stops_a_run_cleanly_with_the_offsets_restored_and_the_shutdown_recorded() {
    let fake = interrupted();
    let (exit, out, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("session.start"), "{err}");
    let events = fake.events();
    let last = events.last().unwrap();
    assert!(
        matches!(&last.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal),
        "{}",
        last.kind().as_str()
    );
    assert_eq!(fake.opened.get(), 1);
}

#[test]
fn a_journal_with_a_newer_evidence_epoch_is_refused_before_the_config_is_read() {
    let mut fake = Fake::new();
    seed_newer_epoch(&mut fake);
    let path = fake.state_dir().join("events.jsonl");
    let before = fs::read(&path).unwrap();
    let opened = fake.opened.get();

    let missing = fake.dir.join("missing.json");
    let (exit, _, err) = fake.run(&["run", "--config", missing.to_str().unwrap()]);
    assert_eq!(exit, Exit::Incompatible, "{err}");
    assert!(err.contains("evidence epoch"), "{err}");
    assert_eq!(fs::read(&path).unwrap(), before);
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
    let held = HostLock::acquire(&fake.lock, Policy::Private).unwrap();
    let grubenv = fake.grubenv();
    let (exit, _, err) = fake.run(&["run", "--tuning-boot", grubenv.to_str().unwrap()]);
    drop(held);
    assert_eq!(exit, Exit::Locked, "{err}");
    assert_eq!(fake.grub.state().cleared, 0);
}

#[test]
fn an_invalid_config_is_a_usage_error_for_a_compatible_journal() {
    let fake = Fake::new();
    let config = fake.dir.join("config.json");
    fs::write(&config, r#"{"bogus": 1}"#).unwrap();
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

#[test]
fn the_config_flag_may_precede_the_command_name() {
    let fake = Fake::new();
    let config = fake.dir.join("config.json");
    fs::write(&config, r#"{"bogus": 1}"#).unwrap();
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
    let held = HostLock::acquire(&fake.lock, Policy::Private).unwrap();
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
    let held = StateDir::lock(fake.state_dir()).unwrap();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    drop(held);
    assert_eq!(exit, Exit::Locked, "{err}");
    assert_eq!(fake.opened.get(), 0);
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
