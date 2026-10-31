//! `togi night`, `togi night-end` and the dashboard's F10, on an in-memory GRUB environment and
//! filesystem.

#![expect(
    clippy::unwrap_used,
    reason = "the helpers panic on a broken fixture, as the tests do"
)]

use crate::support;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use support::{Host, Ran};
use test_case::test_case;
use togi_cli::Exit;
use togi_cli::cli::Context;
use togi_cli::dashboard::Surface;
use togi_cli::system::System;
use togi_config::Config;
use togi_hardware::{Diagnosis, RebootError, StopError};
use togi_hostlock::{HostLock, Policy};
use togi_journal::fs::Fs;
use togi_machine::{BiosContext, BootId, Machine};
use togi_render::OutputFacts;
use togi_tuningboot::night::{DAY_ENTRY_VARIABLE, NIGHT_BEGUN, NIGHT_VARIABLE, SETUP_PATH};
use togi_tuningboot::{Bootloader, Environment, SavedEntry};
use togi_watch::screen::Direction;

const SETUP: &str = r#"{"loader":"grub","grubenv":"/boot/grub/grubenv","entry":"NixOS - togi"}"#;
const DAY: &str = "NixOS - Configuration 7";
const TUNING: &str = "NixOS - togi";
const START: &str = "2026-01-01T00:00:00Z";

type Values = Rc<RefCell<BTreeMap<String, String>>>;

/// GRUB's environment block in memory, restoring the day's entry as the real one does, which
/// `togi-hardware` tests against `grub-editenv`.
struct Grub(Values);

impl Environment for Grub {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.borrow().get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        for (name, value) in values {
            self.0
                .borrow_mut()
                .insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        for name in names {
            self.0.borrow_mut().remove(*name);
        }
        Ok(())
    }
}

impl Bootloader for Grub {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        let mut values = self.0.borrow_mut();
        let before = values.get("saved_entry").cloned().unwrap_or_default();
        let day = values.get(DAY_ENTRY_VARIABLE).cloned();
        let begun = values.contains_key(NIGHT_VARIABLE);
        let after = match (day, begun) {
            (Some(day), true) => {
                values.insert("saved_entry".to_owned(), day.clone());
                values.remove(NIGHT_VARIABLE);
                day
            }
            (Some(day), false) if day == before => day,
            (Some(_) | None, _) => {
                for name in ["saved_entry", NIGHT_VARIABLE, DAY_ENTRY_VARIABLE] {
                    values.remove(name);
                }
                String::new()
            }
        };
        Ok(SavedEntry { before, after })
    }
}

/// The host fake with a clock that moves, a GRUB that persists and a tuner that can refuse to stop.
struct Night {
    host: Host,
    now: Cell<Timestamp>,
    grub: Values,
    answers: RefCell<VecDeque<Option<String>>>,
    stop_fails: Cell<bool>,
    root: Cell<bool>,
    firmware: RefCell<BiosContext>,
}

impl Night {
    fn new() -> Self {
        let night = Self::bare();
        night.host.write(SETUP_PATH, SETUP).unwrap();
        night
    }

    /// A host the module has not set up.
    fn bare() -> Self {
        Self::on(Host::new())
    }

    fn on(host: Host) -> Self {
        Self {
            host,
            now: Cell::new(START.parse().unwrap()),
            grub: Values::default(),
            answers: RefCell::default(),
            stop_fails: Cell::new(false),
            root: Cell::new(true),
            firmware: RefCell::new(BiosContext {
                bios_version: "TEST.1".into(),
                board: "test board".into(),
                cpu_model: "test CPU".into(),
                microcode: "0x1".into(),
                boost_limit_mhz: 5700,
            }),
        }
    }

    fn answer(&self, line: Option<&str>) {
        self.answers.borrow_mut().push_back(line.map(str::to_owned));
    }

    fn day_is(&self, entry: &str) {
        self.grub
            .borrow_mut()
            .insert("saved_entry".to_owned(), entry.to_owned());
    }

    fn value(&self, name: &str) -> Option<String> {
        self.grub.borrow().get(name).cloned()
    }

    fn steps(&self) -> Vec<String> {
        self.host.steps()
    }

    fn reboots(&self) -> usize {
        self.steps().iter().filter(|step| *step == "reboot").count()
    }

    fn with_context<T>(&self, work: impl FnOnce(&mut Context<'_>) -> T) -> (T, Ran) {
        self.with_output(Vec::new(), work)
    }

    fn with_output<T>(
        &self,
        mut out: impl Write,
        work: impl FnOnce(&mut Context<'_>) -> T,
    ) -> (T, Ran) {
        let state = support::MemoryDir::new();
        let mut err = Vec::new();
        let mut ctx = Context {
            state_dir: state.path(),
            out: &mut out,
            err: &mut err,
            system: self,
        };
        let result = work(&mut ctx);
        (
            result,
            Ran {
                exit: Exit::Ok,
                out: String::new(),
                err: String::from_utf8(err).unwrap(),
            },
        )
    }
}

impl System for Night {
    fn check_platform(&self) -> Result<(), togi_hardware::Error> {
        self.host.check_platform()
    }

    fn privileged(&self) -> bool {
        self.root.get()
    }

    fn host_lock(&self) -> (&Path, Policy) {
        self.host.host_lock()
    }

    fn listen(
        &self,
        token: &togi_machine::CancellationToken,
    ) -> Result<Box<dyn std::any::Any>, togi_cli::signals::Error> {
        self.host.listen(token)
    }

    fn acquire_host_lock(&self) -> Result<HostLock, togi_hostlock::Error> {
        self.host.acquire_host_lock()
    }

    fn journal_fs(&self) -> Option<Arc<dyn Fs>> {
        self.host.journal_fs()
    }

    fn stdout_facts(&self) -> OutputFacts {
        self.host.stdout_facts()
    }

    fn config_path(&self) -> &Path {
        self.host.config_path()
    }

    fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
        self.host.boot_id()
    }

    fn now(&self) -> Timestamp {
        self.now.get()
    }

    fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
        self.host.monotonic()
    }

    fn zone(&self) -> TimeZone {
        TimeZone::UTC
    }

    fn topology(&self) -> Result<Vec<togi_machine::CoreInfo>, togi_hardware::Error> {
        self.host.topology()
    }

    fn diagnose(
        &self,
        config: &Config,
        recorded: Option<&BiosContext>,
        lock: Option<&HostLock>,
    ) -> Result<Diagnosis, togi_hardware::Error> {
        self.host.diagnose(config, recorded, lock)
    }

    fn diagnose_day(
        &self,
        config: &Config,
        lock: Option<&HostLock>,
    ) -> Result<Diagnosis, togi_hardware::Error> {
        self.host.diagnose_day(config, lock)
    }

    fn firmware_context(&self, _: &HostLock) -> Result<BiosContext, togi_machine::Error> {
        Ok(self.firmware.borrow().clone())
    }

    fn bootloader(&self, _: &Path) -> io::Result<Box<dyn Bootloader>> {
        self.host.step("grub")?;
        Ok(Box::new(Grub(Rc::clone(&self.grub))))
    }

    fn create_retry_marker(&self) -> io::Result<()> {
        self.host.create_retry_marker()
    }

    fn remove_retry_marker(&self) -> io::Result<()> {
        self.host.remove_retry_marker()
    }

    fn reboot(&self) -> Result<(), RebootError> {
        self.host.reboot()
    }

    fn stop_tuning(&self) -> Result<(), StopError> {
        self.host.step("stop tuning").unwrap();
        if self.stop_fails.get() {
            return Err(StopError::Exit {
                status: ExitStatusExt::from_raw(1 << 8),
                stderr: "denied".into(),
            });
        }
        Ok(())
    }

    fn answer(&self) -> io::Result<Option<String>> {
        self.host.step("ask").unwrap();
        Ok(self.answers.borrow_mut().pop_front().flatten())
    }

    fn prompt_line(&self) -> io::Result<Option<String>> {
        self.host.prompt_line()
    }

    fn day_observer<'lock>(
        &self,
        lock: &'lock HostLock,
    ) -> Result<Box<dyn togi_hardware::day::Observer + 'lock>, togi_machine::Error> {
        self.host.day_observer(lock)
    }

    fn day_request(
        &self,
        request: &togi_cli::day::socket::Request,
    ) -> Result<togi_cli::day::socket::Reply, togi_cli::day::socket::Error> {
        if let togi_cli::day::socket::Request::AttestStock(attest) = request
            && let Some(journal) = &mut *self.host.day_journal.borrow_mut()
        {
            use togi_cli::day::socket::{Control, Request};
            return togi_cli::day::socket::respond(
                journal,
                &togi_cli::system::SystemClock(self),
                0,
                Request::AttestStock(*attest),
                &mut |journal, clock, control| {
                    assert_eq!(control, Control::AttestStock);
                    let firmware = self.firmware.borrow().clone();
                    if !journal.events().iter().any(|event| {
                        matches!(&event.payload, togi_journal::Payload::StockAttested(value) if value.firmware == firmware)
                    }) {
                        journal.append(
                            togi_journal::Stamp { time: clock.now(), mono: Some(clock.monotonic()?) },
                            togi_journal::StockAttested { firmware }.into(),
                            &[],
                        )?;
                    }
                    Ok(self.host.day_status.borrow().clone())
                },
            ).map_err(|error| togi_cli::day::socket::Error::Io(io::Error::other(error)));
        }
        self.host.day_request(request)
    }

    fn day_notification_actions(&self) -> bool {
        self.host.day_notification_actions()
    }

    fn day_notification(
        &self,
        invocation: &togi_command::Invocation,
    ) -> Result<togi_command::Output, togi_command::Error> {
        self.host.day_notification(invocation)
    }

    fn stderr_facts(&self) -> OutputFacts {
        self.host.stderr_facts()
    }

    fn open_machine<'lock>(
        &self,
        config: &Config,
        state: &Path,
        lock: &'lock HostLock,
    ) -> Result<Machine<'lock>, togi_hardware::Error> {
        self.host.open_machine(config, state, lock)
    }

    fn interactive(&self) -> bool {
        self.host.interactive()
    }

    fn stderr(&self) -> Box<dyn Write> {
        self.host.stderr()
    }

    fn surface(&self) -> Arc<dyn Surface> {
        self.host.surface()
    }
}

impl Night {
    fn togi(&self, args: &[&str]) -> Ran {
        self.togi_in(support::MemoryDir::new().path(), args)
    }

    fn togi_in(&self, state: &Path, args: &[&str]) -> Ran {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = togi_cli::run_with(
            ["togi", "--state-dir", state.to_str().unwrap()]
                .into_iter()
                .chain(args.iter().copied()),
            &mut out,
            &mut err,
            self,
        );
        Ran {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }
}

#[test]
fn confirming_selects_the_tuning_entry_remembers_the_day_and_reboots() {
    let night = Night::new();
    night.day_is(DAY);
    night.answer(Some("y\n"));
    let ran = night.togi(&["night"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(night.value("saved_entry").as_deref(), Some(TUNING));
    assert_eq!(night.value(DAY_ENTRY_VARIABLE).as_deref(), Some(DAY));
    assert_eq!(night.value(NIGHT_VARIABLE).as_deref(), Some(NIGHT_BEGUN));
    assert_eq!(night.reboots(), 1);
    assert!(ran.out.contains("no tuning session yet"), "{}", ran.out);
    assert!(
        ran.out
            .contains("stable at CO 0 with BIOS at stock, and reboot into the night now? [y/N]"),
        "{}",
        ran.out
    );
    assert!(ran.out.contains("Good night."), "{}", ran.out);
    assert!(ran.out.contains("runs until you end it"), "{}", ran.out);
    let steps = night.steps();
    let ask = steps.iter().position(|step| step == "ask").unwrap();
    let reboot = steps.iter().position(|step| step == "reboot").unwrap();
    assert!(ask < reboot, "{steps:?}");
}

#[test_case::test_case(Some("n\n"); "no")]
#[test_case::test_case(Some("\n"); "empty line")]
#[test_case::test_case(Some("yes please\n"); "not an answer")]
#[test_case::test_case(None; "input ended")]
fn declining_changes_nothing(answer: Option<&str>) {
    let night = Night::new();
    night.day_is(DAY);
    night.answer(answer);
    let before = night.grub.borrow().clone();
    let ran = night.togi(&["night"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(*night.grub.borrow(), before);
    assert_eq!(night.reboots(), 0);
    assert!(ran.out.contains("Nothing changed."), "{}", ran.out);
    assert!(!ran.out.contains("Good night."), "{}", ran.out);
}

#[test]
fn the_night_has_no_end_time_to_give() {
    let night = Night::new();
    let ran = night.togi(&["night", "--until", "06:30"]);
    assert_eq!(ran.exit, Exit::Usage);
    assert!(night.grub.borrow().is_empty());
    assert_eq!(night.reboots(), 0);
}

#[test]
fn the_night_needs_root_and_asks_nothing_without_it() {
    let night = Night::new();
    night.root.set(false);
    let ran = night.togi(&["night"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(ran.err.contains("needs root"), "{}", ran.err);
    assert!(!night.steps().contains(&"ask".to_owned()));
}

#[test]
fn a_machine_without_the_setup_file_says_so() {
    let night = Night::bare();
    let ran = night.togi(&["night"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(ran.err.contains("no tuning boot is set up"), "{}", ran.err);
    assert_eq!(night.reboots(), 0);
}

#[test]
fn a_failed_reboot_gives_the_day_back() {
    let night = Night::new();
    night.day_is(DAY);
    night.answer(Some("y\n"));
    night.host.fail.borrow_mut().push("reboot");
    let ran = night.togi(&["night"]);
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
    assert_eq!(night.value(NIGHT_VARIABLE), None);
}

#[test_case(&["night"]; "begin")]
#[test_case(&["night-end"]; "end")]
fn an_unavailable_bootloader_changes_no_selection_and_never_reboots(args: &[&str]) {
    let night = Night::new();
    night.day_is(DAY);
    night.answer(Some("y\n"));
    let before = night.grub.borrow().clone();
    night.host.fail.borrow_mut().push("grub");
    let ran = night.togi(args);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(*night.grub.borrow(), before);
    assert_eq!(night.reboots(), 0);
}

fn night_begun(night: &Night) {
    night.day_is(DAY);
    night.answer(Some("y\n"));
    let ran = night.togi(&["night"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
}

#[test]
fn night_end_restores_the_day_exactly_and_repeating_it_changes_nothing() {
    let night = Night::new();
    night_begun(&night);
    for _ in 0..2 {
        let ran = night.togi(&["night-end"]);
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
        assert_eq!(night.value(NIGHT_VARIABLE), None);
    }
    assert_eq!(night.reboots(), 1, "only the night's own reboot");
}

#[test]
fn night_end_after_a_menu_picked_tuning_boot_returns_to_the_boot_loaders_default() {
    let night = Night::new();
    night_begun(&night);
    assert_eq!(night.togi(&["night-end"]).exit, Exit::Ok);
    // A later tuning boot picked from the menu: GRUB's savedefault selects it, nothing begins a night.
    night.day_is(TUNING);
    let ran = night.togi(&["night-end"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(night.value("saved_entry"), None);
    assert_eq!(night.value(DAY_ENTRY_VARIABLE), None);
}

#[test]
fn leaving_from_the_dashboard_stops_the_run_then_restores_then_reboots() {
    let night = Night::new();
    night_begun(&night);
    let before = night.steps().len();
    let (exit, _) = night.with_context(togi_cli::night_end::leave);
    assert_eq!(exit, Exit::Ok);
    let steps = night.steps()[before..].to_vec();
    assert_eq!(steps, ["stop tuning", "grub", "reboot"]);
    assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
    assert_eq!(night.value(NIGHT_VARIABLE), None);
}

#[test]
fn a_run_that_will_not_stop_leaves_the_night_as_it_was() {
    let night = Night::new();
    night_begun(&night);
    let before = night.grub.borrow().clone();
    let reboots = night.reboots();
    night.stop_fails.set(true);
    let (exit, ran) = night.with_context(togi_cli::night_end::leave);
    assert_eq!(exit, Exit::Error);
    assert!(ran.err.contains("stop the run"), "{}", ran.err);
    assert_eq!(*night.grub.borrow(), before);
    assert_eq!(night.reboots(), reboots);
}

#[test]
fn the_night_shows_the_plan_status_gives_for_the_session() {
    let session = support::Session::new();
    let night = Night::on(session.host());
    night.host.write(SETUP_PATH, SETUP).unwrap();
    night.answer(Some("n\n"));
    let plan = night.togi_in(session.path(), &["status"]).out;
    let header = plan.lines().next().unwrap().to_owned();
    assert!(!header.is_empty(), "{plan}");
    let ran = night.togi_in(session.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(
        ran.out.contains(&format!("tonight: {header}")),
        "{header}\n{}",
        ran.out
    );
    assert!(!ran.out.contains("no tuning session yet"), "{}", ran.out);
}

/// Standard output that breaks once the good-night message comes, as a closed pipe would.
struct BreaksAtGoodnight;

impl Write for BreaksAtGoodnight {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.windows(10).any(|window| window == b"Good night") {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_good_night_message_that_cannot_be_written_gives_the_tuning_selection_back() {
    let night = Night::new();
    night.day_is(DAY);
    let (exit, _) = night.with_output(BreaksAtGoodnight, |ctx| {
        togi_cli::night::begin(ctx, togi_cli::night::Confirmation::Given)
    });
    assert_eq!(exit, Exit::Error);
    assert_eq!(night.reboots(), 0);
    assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
    assert_eq!(night.value(NIGHT_VARIABLE), None);
}

#[derive(clap::Parser)]
struct WatchCli {
    #[command(flatten)]
    watch: togi_cli::watch::Args,
}

fn offered(night: &Night, args: &[&str]) -> Option<Direction> {
    let args = <WatchCli as clap::Parser>::try_parse_from(
        std::iter::once("watch").chain(args.iter().copied()),
    )
    .unwrap()
    .watch;
    night
        .with_context(|ctx| togi_cli::watch::offer(&args, ctx))
        .0
}

#[test]
fn the_dashboard_offers_the_night_only_to_root_in_a_normal_boot_with_a_tuning_boot_set_up() {
    let night = Night::new();
    assert_eq!(offered(&night, &[]), Some(Direction::Night));
    night.root.set(false);
    assert_eq!(offered(&night, &[]), None);
}

#[test]
fn the_tuning_boot_dashboard_is_offered_only_the_way_back_to_the_day() {
    let night = Night::new();
    assert_eq!(offered(&night, &["--tuning-boot"]), Some(Direction::Day));
    night.root.set(false);
    assert_eq!(offered(&night, &["--tuning-boot"]), None);
}

#[test_case(&[]; "normal boot")]
#[test_case(&["--tuning-boot"]; "tuning boot")]
fn a_host_without_a_tuning_boot_set_up_offers_no_switch(args: &[&str]) {
    assert_eq!(offered(&Night::bare(), args), None);
}

fn attestations(night: &Night, state: &Path) -> Vec<BiosContext> {
    night
        .host
        .recording(&state.join("events.jsonl"))
        .unwrap()
        .into_events()
        .into_iter()
        .filter_map(|event| {
            if let togi_journal::Payload::StockAttested(attested) = event.payload {
                Some(attested.firmware)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn a_fresh_night_durably_attests_once_per_firmware_context() {
    let night = Night::new();
    let state = support::MemoryDir::new();
    let expected = night.firmware.borrow().clone();
    night.answer(Some("yes\n"));
    assert_eq!(night.togi_in(state.path(), &["night"]).exit, Exit::Ok);
    assert_eq!(
        attestations(&night, state.path()),
        std::slice::from_ref(&expected)
    );
    let recording = night
        .host
        .recording(&state.path().join("events.jsonl"))
        .unwrap();
    assert!(matches!(
        recording.events()[0].payload,
        togi_journal::Payload::SessionStart(_)
    ));
    assert_eq!(
        recording.events().len(),
        2,
        "run still owns context and baseline observation"
    );
    night.answer(Some("yes\n"));
    assert_eq!(night.togi_in(state.path(), &["night"]).exit, Exit::Ok);
    assert_eq!(attestations(&night, state.path()), [expected]);
}

#[test]
fn a_reset_requires_a_fresh_stock_attestation_even_with_unchanged_firmware() {
    let night = Night::new();
    let state = support::MemoryDir::new();
    let path = state.path().join("events.jsonl");
    night.answer(Some("yes\n"));
    assert_eq!(night.togi_in(state.path(), &["night"]).exit, Exit::Ok);
    let original = night.host.recording(&path).unwrap();
    assert_eq!(attestations(&night, state.path()).len(), 1);

    let reset = night.togi_in(state.path(), &["reset", "--all"]);
    assert_eq!(reset.exit, Exit::Ok, "{}", reset.err);
    night.now.set(night.now.get() + Duration::from_secs(1));
    night.answer(Some("no\n"));
    let declined = night.togi_in(state.path(), &["night"]);
    assert_eq!(declined.exit, Exit::Ok, "{}", declined.err);
    assert!(declined.out.contains("stable at CO 0 with BIOS at stock"));
    assert!(night.host.fs.kind(&path).unwrap().is_none());

    night.answer(Some("yes\n"));
    let confirmed = night.togi_in(state.path(), &["night"]);
    assert_eq!(confirmed.exit, Exit::Ok, "{}", confirmed.err);
    let current = night.host.recording(&path).unwrap();
    assert_ne!(current.events()[0].payload, original.events()[0].payload);
    assert_eq!(
        attestations(&night, state.path()),
        [night.firmware.borrow().clone()]
    );
    assert_eq!(current.events().len(), 2, "old consent was not imported");
}

#[test_case::test_case("bios"; "BIOS or AGESA change")]
#[test_case::test_case("microcode"; "microcode change")]
fn changed_firmware_requires_and_records_the_confirmation_again(field: &str) {
    let night = Night::new();
    let state = support::MemoryDir::new();
    night.answer(Some("yes\n"));
    assert_eq!(night.togi_in(state.path(), &["night"]).exit, Exit::Ok);
    let before = night.firmware.borrow().clone();
    match field {
        "bios" => night
            .firmware
            .borrow_mut()
            .bios_version
            .push_str("-next-AGESA"),
        "microcode" => night.firmware.borrow_mut().microcode.push_str("-next"),
        _ => unreachable!("test cases name a firmware field"),
    }
    night.answer(Some("no\n"));
    let declined = night.togi_in(state.path(), &["night"]);
    assert_eq!(declined.exit, Exit::Ok, "{}", declined.err);
    assert!(declined.out.contains("stable at CO 0 with BIOS at stock"));
    assert_eq!(
        attestations(&night, state.path()),
        std::slice::from_ref(&before)
    );
    assert_eq!(night.reboots(), 1);
    night.answer(Some("yes\n"));
    let confirmed = night.togi_in(state.path(), &["night"]);
    assert_eq!(confirmed.exit, Exit::Ok, "{}", confirmed.err);
    assert!(confirmed.out.contains("stable at CO 0 with BIOS at stock"));
    assert_eq!(
        attestations(&night, state.path()),
        [before.clone(), night.firmware.borrow().clone()]
    );
    assert_eq!(night.reboots(), 2);
    *night.firmware.borrow_mut() = before;
    night.answer(Some("yes\n"));
    assert_eq!(night.togi_in(state.path(), &["night"]).exit, Exit::Ok);
    assert_eq!(
        attestations(&night, state.path()).len(),
        2,
        "returning firmware reuses its original attestation"
    );
}

#[test_case::test_case(togi_journal::fs::Op::Write, 2; "attestation write")]
#[test_case::test_case(togi_journal::fs::Op::Sync, 3; "attestation durability")]
fn failed_attestation_recording_never_selects_or_reboots_the_tuning_boot(
    operation: togi_journal::fs::Op,
    nth: u64,
) {
    let night = Night::new();
    let state = support::MemoryDir::new();
    night.day_is(DAY);
    night.answer(Some("yes\n"));
    night
        .host
        .fs
        .inject(togi_journal::fs::Fault::before(operation, nth).named("events.jsonl"));
    let ran = night.togi_in(state.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
    assert_eq!(night.value(NIGHT_VARIABLE), None);
    assert_eq!(night.reboots(), 0);
}

#[test]
fn dashboard_confirmation_records_the_same_attestation_without_another_question() {
    let night = Night::new();
    let (exit, ran) = night.with_context(|ctx| {
        let result = togi_cli::night::begin(ctx, togi_cli::night::Confirmation::Given);
        assert_eq!(
            attestations(&night, ctx.state_dir),
            [night.firmware.borrow().clone()]
        );
        result
    });
    assert_eq!(exit, Exit::Ok, "{}", ran.err);
    assert!(!night.steps().contains(&"ask".to_owned()));
    assert_eq!(night.reboots(), 1);
}

#[test]
fn declining_a_fresh_night_creates_no_session_or_attestation() {
    let night = Night::new();
    let state = support::MemoryDir::new();
    night.answer(Some("no\n"));
    assert_eq!(night.togi_in(state.path(), &["night"]).exit, Exit::Ok);
    assert_eq!(night.host.recorded_build(state.path()).unwrap(), None);
}

#[test]
fn a_live_day_writer_records_the_night_confirmation_without_competing_for_its_locks() {
    let session = support::Session::new();
    let night = Night::on(session.host());
    night.host.write(SETUP_PATH, SETUP).unwrap();
    let _host_lock = night.host.acquire_host_lock().unwrap();
    let journal = night
        .host
        .lock_state_dir(session.path())
        .unwrap()
        .open_at(night.boot_id().unwrap(), night.now())
        .unwrap();
    *night.host.day_journal.borrow_mut() = Some(journal);
    night.answer(Some("yes\n"));
    let ran = night.togi_in(session.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(attestations(&night, session.path()).contains(&night.firmware.borrow().clone()));
    assert_eq!(night.reboots(), 1);
    assert!(
        night.host.day_journal.borrow().is_some(),
        "observer keeps ownership through reboot"
    );
}

#[test]
fn a_live_day_writer_append_failure_prevents_the_tuning_reboot() {
    let session = support::Session::new();
    let night = Night::on(session.host());
    night.host.write(SETUP_PATH, SETUP).unwrap();
    let _host_lock = night.host.acquire_host_lock().unwrap();
    let journal = night
        .host
        .lock_state_dir(session.path())
        .unwrap()
        .open_at(night.boot_id().unwrap(), night.now())
        .unwrap();
    *night.host.day_journal.borrow_mut() = Some(journal);
    night.day_is(DAY);
    night.host.fs.inject(
        togi_journal::fs::Fault::before(togi_journal::fs::Op::Write, 1).named("events.jsonl"),
    );
    night.answer(Some("yes\n"));
    let ran = night.togi_in(session.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
    assert_eq!(night.reboots(), 0);
}

/// A night already confirmed once, with firmware that has since changed, so a new attestation is owed.
fn night_owing_a_new_attestation() -> (Night, support::MemoryDir) {
    let night = Night::new();
    let state = support::MemoryDir::new();
    night.answer(Some("yes\n"));
    assert_eq!(night.togi_in(state.path(), &["night"]).exit, Exit::Ok);
    night.firmware.borrow_mut().bios_version = "TEST.2".into();
    night.now.set(night.now.get() + Duration::from_secs(1));
    (night, state)
}

/// Everything a refused night must leave alone: the journal, the boot selection and the reboot count.
fn refusal_surface(
    night: &Night,
    state: &Path,
) -> (usize, Vec<BiosContext>, Option<String>, Option<String>, usize) {
    (
        night
            .host
            .recording(&state.join("events.jsonl"))
            .unwrap()
            .events()
            .len(),
        attestations(night, state),
        night.value("saved_entry"),
        night.value(NIGHT_VARIABLE),
        night.reboots(),
    )
}

#[test]
fn a_busy_host_lock_refuses_the_night_with_the_contention_status_and_changes_nothing() {
    let (night, state) = night_owing_a_new_attestation();
    let before = refusal_surface(&night, state.path());
    let _held = night.host.acquire_host_lock().unwrap();
    night.answer(Some("yes\n"));
    let ran = night.togi_in(state.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    assert!(
        ran.err.contains("another togi process holds the host lock"),
        "{}",
        ran.err
    );
    assert_eq!(refusal_surface(&night, state.path()), before);
}

#[test]
fn a_host_lock_that_cannot_be_taken_fails_the_night_with_its_cause_and_changes_nothing() {
    let (night, state) = night_owing_a_new_attestation();
    let before = refusal_surface(&night, state.path());
    *night.host.lock_error.borrow_mut() = Some(togi_hostlock::Error::Io {
        path: night.host.lock.clone(),
        operation: togi_hostlock::Operation::Lock,
        source: io::Error::other("disk gone"),
    });
    night.answer(Some("yes\n"));
    let ran = night.togi_in(state.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert!(ran.err.contains("disk gone"), "{}", ran.err);
    assert_eq!(refusal_surface(&night, state.path()), before);
}

#[test]
fn a_held_state_writer_lock_refuses_the_night_with_the_contention_status_and_changes_nothing() {
    let (night, state) = night_owing_a_new_attestation();
    let before = refusal_surface(&night, state.path());
    let held = night.host.lock_state_dir(state.path()).unwrap();
    night.answer(Some("yes\n"));
    let ran = night.togi_in(state.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    assert!(
        ran.err.contains("another togi process holds the journal lock"),
        "{}",
        ran.err
    );
    drop(held);
    assert_eq!(refusal_surface(&night, state.path()), before);
}

#[test]
fn a_state_writer_lock_that_cannot_be_taken_fails_the_night_and_changes_nothing() {
    let (night, state) = night_owing_a_new_attestation();
    let before = refusal_surface(&night, state.path());
    night
        .host
        .fs
        .inject(togi_journal::fs::Fault::before(togi_journal::fs::Op::Open, 1).named("lock"));
    night.answer(Some("yes\n"));
    let ran = night.togi_in(state.path(), &["night"]);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(refusal_surface(&night, state.path()), before);
}

#[test]
fn a_fresh_night_confirmation_is_used_by_the_later_run_after_verified_stock_startup() {
    let definition = togi_sim::MachineDefinition::default();
    let sim = togi_sim::Simulator::new(
        togi_sim::Model::new(&definition, 1).unwrap(),
        togi_sim::RunSettings {
            seed: 1,
            ..togi_sim::RunSettings::default()
        },
    )
    .unwrap();
    let mut machine = sim.seams();
    let mut host = Host::new();
    host.topology = machine.host.topology().unwrap();
    let night = Night::on(host);
    *night.firmware.borrow_mut() = machine.host.bios_context().unwrap();
    night.host.write(SETUP_PATH, SETUP).unwrap();
    let state = support::MemoryDir::new();
    night.answer(Some("yes\n"));
    let confirmed = night.togi_in(state.path(), &["night"]);
    assert_eq!(confirmed.exit, Exit::Ok, "{}", confirmed.err);
    let boot = machine.host.boot_id().unwrap();
    let journal = night
        .host
        .lock_state_dir(state.path())
        .unwrap()
        .open_at(boot.clone(), sim.now())
        .unwrap();
    let cancel = togi_machine::CancellationToken::new();
    let stop = cancel.clone();
    let ended = togi_session::run(togi_session::RunInput {
        config: Config::default(),
        source: togi_session::ConfigSource::default(),
        build: togi_cli::report::current_build(),
        boot,
        journal,
        machine,
        publication: togi_session::PublicationPolicy::OnFinish,
        bootloader: None,
        defects: &[],
        resume: None,
        import: None,
        log: None,
        on_append: Some(Box::new(move |event| {
            if matches!(event.payload, togi_journal::Payload::TrialEnd(_)) {
                stop.cancel();
            }
        })),
        renderer: togi_render::Renderer::default(),
        zone: TimeZone::UTC,
        cancel,
        report: Box::new(io::sink()),
    });
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let events = ended.journal.events();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, togi_journal::Payload::CrashDetected(_))),
        "the normal boot's session envelope and stock confirmation are not tuning activity"
    );
    let attested = events
        .iter()
        .position(|event| matches!(event.payload, togi_journal::Payload::StockAttested(_)))
        .unwrap();
    let context = events
        .iter()
        .position(|event| matches!(event.payload, togi_journal::Payload::SessionContext(_)))
        .unwrap();
    let baseline = events
        .iter()
        .position(|event| matches!(event.payload, togi_journal::Payload::SessionBaseline(_)))
        .unwrap();
    let first = events
        .iter()
        .position(|event| matches!(event.payload, togi_journal::Payload::TrialIntent(_)))
        .unwrap();
    assert!(attested < context && context < baseline && baseline < first);
    let togi_journal::Payload::SessionBaseline(recorded) = &events[baseline].payload else {
        unreachable!()
    };
    assert!(recorded.offsets.iter().all(|(_, offset)| offset.0 == 0));
    let trials: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let togi_journal::Payload::TrialIntent(trial) = &event.payload {
                Some(trial)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(trials[0].offset.unwrap().get(), -5);
    assert!(
        trials
            .iter()
            .all(|trial| trial.profile.iter().any(|(_, offset)| offset.get() != 0))
    );
    assert_eq!(
        attestations(&night, state.path()),
        [night.firmware.borrow().clone()]
    );
}
