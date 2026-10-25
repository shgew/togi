//! `togi night`, `togi night-end` and the dashboard's F10, on an in-memory GRUB environment and
//! filesystem.

#![expect(
    clippy::unwrap_used,
    reason = "the helpers panic on a broken fixture, as the tests do"
)]

#[path = "../support/mod.rs"]
mod support;

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
        ran.out.contains("Reboot into the night now? [y/N]"),
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
    let header = plan
        .lines()
        .find(|line| line.contains("phase"))
        .unwrap()
        .to_owned();
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
