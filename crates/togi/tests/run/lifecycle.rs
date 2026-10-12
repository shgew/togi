//! `togi run` in process, against a simulated machine: the order of its steps, what refuses it before
//! hardware, and how each end of the session maps to a status. The Go tests these replace are listed in
//! `docs/rust-migration.md`.

use std::fs;

use togi_cli::Exit;
use togi_hostlock::{HostLock, Policy};
use togi_journal::{EventKind, Payload, ShutdownReason, StateDir};
use togi_machine::Machine;

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

fn append_future_kind(fake: &Fake) -> Vec<u8> {
    let path = fake.state_dir().join("events.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    let next = text.lines().count() + 1;
    text.push_str(&r#"{"seq":N,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"future.thing","msg":"x","data":{}}"#.replace('N', &next.to_string()));
    text.push('\n');
    fs::write(&path, &text).unwrap();
    text.into_bytes()
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
    assert!(
        err.contains(r#"unknown event kind "future.thing""#),
        "{err}"
    );
    assert!(err.contains("install the build that wrote it"), "{err}");
    assert_eq!(
        fs::read(fake.state_dir().join("events.jsonl")).unwrap(),
        before
    );
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

#[test]
fn a_journal_that_is_not_a_journal_is_an_error_and_is_left_alone() {
    let fake = Fake::new();
    fs::create_dir_all(fake.state_dir()).unwrap();
    let path = fake.state_dir().join("events.jsonl");
    fs::write(&path, "not json\n").unwrap();
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert!(err.starts_with("togi run: "), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "not json\n");
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
    if rustix::process::geteuid().is_root() {
        return;
    }
    let mut fake = Fake::new();
    fake.policy = Policy::Host;
    let (exit, _, err) = fake.run(&["run", "--no-tui"]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert!(err.starts_with("togi run: host lock"), "{err}");
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
    assert!(fake.events().is_empty());
}

#[test]
fn the_bios_context_is_not_read_from_a_machine_whose_identity_is_not_validated() {
    let bios_reads = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut fake = Fake::new();
    let seen = bios_reads.clone();
    fake.wrap = Box::new(move |machine| Machine {
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
fn the_dashboard_starts_after_the_journal_is_open_and_after_the_last_event_of_the_earlier_run() {
    let mut first = Fake::new();
    first_run(&mut first);
    let earlier = first.events().len() as u64;

    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let record = seen.clone();
    let mut fake = Fake::in_dir(first.sim.clone(), &first.dir);
    fake.interactive = true;
    fake.wrap = Box::new(interrupt_on_first_write);
    fake.surface = std::sync::Arc::new(Records { seen: record });
    let (exit, _, err) = fake.run(&["run"]);
    assert_eq!(exit, Exit::Ok, "{err}");
    let (since, journal_existed) = seen.lock().unwrap().unwrap();
    assert!(
        journal_existed,
        "the dashboard started before the journal was open"
    );
    assert_eq!(
        since, earlier,
        "it must say STARTING until an event after the earlier run's last"
    );
}

struct Records {
    seen: std::sync::Arc<std::sync::Mutex<Option<(u64, bool)>>>,
}

impl togi_cli::dashboard::Surface for Records {
    fn draw(
        &self,
        state_dir: &std::path::Path,
        since: u64,
        stop: &togi_cli::dashboard::Stop,
    ) -> Result<(), String> {
        *self.seen.lock().unwrap() = Some((since, state_dir.join("events.jsonl").exists()));
        stop.wait();
        Ok(())
    }
}
