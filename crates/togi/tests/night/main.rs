//! `togi night`, `togi night-guard`, `togi night-end` and the dashboard's F10, on an in-memory GRUB
//! environment, clock and filesystem.

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
use togi_tuningboot::night::{DAY_ENTRY_VARIABLE, NIGHT_VARIABLE, SETUP_PATH};
use togi_tuningboot::{Bootloader, Environment, SavedEntry};
use togi_watch::screen::Direction;

const SETUP: &str = r#"{"grubenv":"/boot/grub/grubenv","entry":"NixOS - togi","morning":"07:00"}"#;
const DAY: &str = "NixOS - Configuration 7";
const TUNING: &str = "NixOS - togi";
const START: &str = "2026-01-01T00:00:00Z";

type Values = Rc<RefCell<BTreeMap<String, String>>>;

/// GRUB's environment block in memory, restoring the day's entry as the real one does.
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
        let after = if let Some(day) = values.get(DAY_ENTRY_VARIABLE).cloned() {
            values.insert("saved_entry".to_owned(), day.clone());
            day
        } else {
            values.remove("saved_entry");
            String::new()
        };
        values.remove(NIGHT_VARIABLE);
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

    fn at(&self, time: &str) {
        self.now.set(time.parse().unwrap());
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

    /// The night record's deadline as written by the command.
    fn until(&self) -> Timestamp {
        let record: serde_json::Value =
            serde_json::from_str(&self.value(NIGHT_VARIABLE).unwrap()).unwrap();
        Timestamp::from_second(record["until"].as_i64().unwrap()).unwrap()
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

    fn bootloader(&self, _: &Path) -> Box<dyn Bootloader> {
        self.host.step("grub").unwrap();
        Box::new(Grub(Rc::clone(&self.grub)))
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
    assert_eq!(night.until(), "2026-01-01T07:00:00Z".parse().unwrap());
    assert_eq!(night.reboots(), 1);
    assert!(ran.out.contains("no tuning session yet"), "{}", ran.out);
    assert!(
        ran.out.contains("Reboot into the night now? [y/N]"),
        "{}",
        ran.out
    );
    assert!(ran.out.contains("Good night."), "{}", ran.out);
    assert!(ran.out.contains("2026-01-01 07:00"), "{}", ran.out);
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

#[test_case::test_case("00:00", "06:30", "2026-01-01T06:30:00Z"; "later tonight")]
#[test_case::test_case("06:45", "06:30", "2026-01-02T06:30:00Z"; "already past today")]
#[test_case::test_case("06:30", "06:30", "2026-01-02T06:30:00Z"; "exactly now")]
fn until_overrides_the_morning(now: &str, until: &str, ends: &str) {
    let night = Night::new();
    night.at(&format!("2026-01-01T{now}:00Z"));
    night.answer(Some("y\n"));
    let ran = night.togi(&["night", "--until", until]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(night.until(), ends.parse().unwrap());
}

#[test]
fn a_bad_until_is_a_usage_error_that_changes_nothing() {
    let night = Night::new();
    let ran = night.togi(&["night", "--until", "25:00"]);
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

fn night_begun_until(night: &Night, until: &str) {
    night.day_is(DAY);
    night.answer(Some("y\n"));
    let ran = night.togi(&["night", "--until", until]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
}

#[test]
fn the_guard_lets_the_night_go_on_before_the_deadline_and_ends_it_after() {
    let night = Night::new();
    night_begun_until(&night, "06:00");
    let reboots = night.reboots();
    night.at("2026-01-01T05:59:59Z");
    let ran = night.togi(&["night-guard"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(night.reboots(), reboots);
    assert_eq!(night.value("saved_entry").as_deref(), Some(TUNING));

    night.at("2026-01-01T06:00:00Z");
    let ran = night.togi(&["night-guard"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(night.reboots(), reboots + 1);
    assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
    assert_eq!(night.value(NIGHT_VARIABLE), None);
    assert!(ran.err.contains("returning to the day"), "{}", ran.err);
}

#[test]
fn the_deadline_survives_crash_reboots_spread_over_the_night() {
    let night = Night::new();
    night_begun_until(&night, "06:00");
    let reboots = night.reboots();
    for hour in 1..6 {
        night.at(&format!("2026-01-01T0{hour}:00:00Z"));
        assert_eq!(night.togi(&["night-guard"]).exit, Exit::Ok);
    }
    assert_eq!(night.reboots(), reboots);
    assert_eq!(night.until(), "2026-01-01T06:00:00Z".parse().unwrap());
    night.at("2026-01-01T06:05:00Z");
    night.togi(&["night-guard"]);
    assert_eq!(night.reboots(), reboots + 1);
    assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
}

#[test]
fn many_boots_in_quick_succession_before_the_morning_keep_the_night() {
    let night = Night::new();
    night_begun_until(&night, "06:00");
    let reboots = night.reboots();
    for minute in 0..40 {
        night.at(&format!("2026-01-01T01:{:02}:00Z", minute + 10));
        assert_eq!(night.togi(&["night-guard"]).exit, Exit::Ok);
    }
    assert_eq!(night.reboots(), reboots);
    assert_eq!(night.value("saved_entry").as_deref(), Some(TUNING));
}

#[test]
fn a_tuning_boot_from_the_menu_gets_the_next_morning_as_its_deadline() {
    let night = Night::new();
    night.at("2026-01-01T23:00:00Z");
    let ran = night.togi(&["night-guard"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(night.until(), "2026-01-02T07:00:00Z".parse().unwrap());
    assert_eq!(night.reboots(), 0);
}

#[test]
fn a_guard_that_cannot_read_its_setup_fails_so_the_tuner_does_not_start() {
    let night = Night::bare();
    assert_eq!(night.togi(&["night-guard"]).exit, Exit::Error);
}

#[test]
fn night_end_restores_the_day_exactly_and_repeating_it_changes_nothing() {
    let night = Night::new();
    night_begun_until(&night, "06:00");
    for _ in 0..2 {
        let ran = night.togi(&["night-end"]);
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        assert_eq!(night.value("saved_entry").as_deref(), Some(DAY));
        assert_eq!(night.value(NIGHT_VARIABLE), None);
    }
    assert_eq!(night.reboots(), 1, "only the night's own reboot");
}

#[test]
fn leaving_from_the_dashboard_stops_the_run_then_restores_then_reboots() {
    let night = Night::new();
    night_begun_until(&night, "06:00");
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
    night_begun_until(&night, "06:00");
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
        togi_cli::night::begin(ctx, None, togi_cli::night::Confirmation::Given)
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
