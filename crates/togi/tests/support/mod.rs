#![allow(
    unreachable_pub,
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the fake host serves several test crates, each using part of it, and panics on a broken fixture"
)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::rc::Rc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tempfile::TempDir;
use togi_cli::Exit;
use togi_cli::system::System;
use togi_config::Config;
use togi_hardware::{Diagnosis, RebootError};
use togi_hostlock::Policy;
use togi_journal::Payload;
use togi_machine::{BiosContext, BootId};
use togi_render::OutputFacts;
use togi_tuningboot::{Bootloader, Environment, SavedEntry};
use togi_watchtest::{Scenario, install};

pub struct Ran {
    pub exit: Exit,
    pub out: String,
    pub err: String,
}

pub type Steps = Rc<RefCell<Vec<String>>>;

pub struct Env {
    pub values: HashMap<String, String>,
    pub steps: Steps,
    pub fail: Vec<&'static str>,
}

impl Env {
    fn step(&self, name: &str) -> io::Result<()> {
        self.steps.borrow_mut().push(name.to_owned());
        if self.fail.contains(&name) {
            return Err(io::Error::other("injected failure"));
        }
        Ok(())
    }
}

impl Environment for Env {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        self.step(&format!("get {name}"))?;
        Ok(self.values.get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        self.step("set")?;
        for (name, value) in values {
            self.values.insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        self.step("unset")?;
        for name in names {
            self.values.remove(*name);
        }
        Ok(())
    }
}

impl Bootloader for Env {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        self.step("clear saved_entry")?;
        Ok(SavedEntry {
            before: "togi".into(),
            after: String::new(),
        })
    }
}

pub struct Host {
    pub linux: bool,
    pub boot_fails: bool,
    pub root: bool,
    pub lock: PathBuf,
    pub boot: String,
    pub steps: Steps,
    pub grubenvs: Rc<RefCell<Vec<PathBuf>>>,
    pub count: RefCell<Option<String>>,
    pub fail: RefCell<Vec<&'static str>>,
    pub marker: Cell<bool>,
    pub marker_at_reboot: Cell<Option<bool>>,
    pub diagnosis: RefCell<Option<Result<Diagnosis, togi_hardware::Error>>>,
    pub diagnoses: RefCell<Vec<(Option<BiosContext>, bool)>>,
    pub calls: Cell<usize>,
    pub facts: OutputFacts,
    _dir: TempDir,
}

impl Host {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("togi-host-")
            .tempdir()
            .unwrap();
        Self {
            linux: true,
            boot_fails: false,
            root: true,
            lock: dir.path().join("togi.lock"),
            boot: "0123456789abcdef0123456789abcdef".into(),
            steps: Steps::default(),
            grubenvs: Rc::default(),
            count: RefCell::new(None),
            fail: RefCell::default(),
            marker: Cell::new(false),
            marker_at_reboot: Cell::new(None),
            diagnosis: RefCell::new(None),
            diagnoses: RefCell::default(),
            calls: Cell::new(0),
            facts: OutputFacts::default(),
            _dir: dir,
        }
    }

    pub fn step(&self, name: &str) -> io::Result<()> {
        self.steps.borrow_mut().push(name.to_owned());
        if self.fail.borrow().contains(&name) {
            return Err(io::Error::other("injected failure"));
        }
        Ok(())
    }

    pub fn steps(&self) -> Vec<String> {
        self.steps.borrow().clone()
    }

    pub fn run_to_closed_stdout(&self, state_dir: &Path, args: &[&str]) -> Ran {
        struct Closed;
        impl io::Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("closed"))
            }
        }
        let mut err = Vec::new();
        let exit = togi_cli::run_with(
            ["togi", "--state-dir", state_dir.to_str().unwrap()]
                .into_iter()
                .chain(args.iter().copied()),
            &mut Closed,
            &mut err,
            self,
        );
        Ran {
            exit,
            out: String::new(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    pub fn run(&self, state_dir: &Path, args: &[&str]) -> Ran {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = togi_cli::run_with(
            ["togi", "--state-dir", state_dir.to_str().unwrap()]
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

impl System for Host {
    fn check_platform(&self) -> Result<(), togi_hardware::Error> {
        if self.linux {
            Ok(())
        } else {
            Err(togi_hardware::Error::Unsupported)
        }
    }

    fn privileged(&self) -> bool {
        self.root
    }

    fn host_lock(&self) -> (&Path, Policy) {
        (&self.lock, Policy::Private)
    }

    fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
        if self.boot_fails {
            return Err(togi_detect::Error::InvalidBootId { id: "none".into() });
        }
        Ok(BootId::from(self.boot.clone()))
    }

    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn monotonic(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn zone(&self) -> TimeZone {
        TimeZone::UTC
    }

    fn diagnose(
        &self,
        _: &Config,
        recorded: Option<&BiosContext>,
        privileged: bool,
    ) -> Result<Diagnosis, togi_hardware::Error> {
        self.calls.set(self.calls.get() + 1);
        self.diagnoses
            .borrow_mut()
            .push((recorded.cloned(), privileged));
        self.diagnosis.borrow_mut().take().unwrap()
    }

    fn bootloader(&self, grubenv: &Path) -> Box<dyn Bootloader> {
        self.grubenvs.borrow_mut().push(grubenv.to_owned());
        let values = self
            .count
            .borrow()
            .iter()
            .map(|count| ("togi_restart_count".to_owned(), count.clone()))
            .collect();
        Box::new(Env {
            values,
            steps: Rc::clone(&self.steps),
            fail: self.fail.borrow().clone(),
        })
    }

    fn create_retry_marker(&self) -> io::Result<()> {
        self.step("create marker")?;
        self.marker.set(true);
        Ok(())
    }

    fn remove_retry_marker(&self) -> io::Result<()> {
        self.step("remove marker")?;
        self.marker.set(false);
        Ok(())
    }

    fn reboot(&self) -> Result<(), RebootError> {
        self.steps.borrow_mut().push("reboot".into());
        self.marker_at_reboot.set(Some(self.marker.get()));
        if self.fail.borrow().contains(&"reboot") {
            return Err(RebootError::Exit {
                status: ExitStatus::from_raw(1 << 8),
                stderr: "denied".into(),
            });
        }
        Ok(())
    }

    fn stderr_facts(&self) -> OutputFacts {
        self.facts
    }
}

/// A state directory holding the simulated journal of a session at its first search trial.
pub struct Session {
    pub dir: TempDir,
    pub context: BiosContext,
}

impl Session {
    pub fn new() -> Self {
        Self::of(Scenario::Confirm)
    }

    pub fn of(scenario: Scenario) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("togi-state-")
            .tempdir()
            .unwrap();
        install(dir.path(), scenario).unwrap();
        let context = scenario
            .events()
            .unwrap()
            .iter()
            .find_map(|event| match &event.payload {
                Payload::SessionContext(context) => Some(context.bios_context()),
                _ => None,
            })
            .expect("the scenario records a BIOS context");
        Self { dir, context }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn journal(&self) -> PathBuf {
        self.path().join("events.jsonl")
    }

    pub fn text(&self) -> String {
        std::fs::read_to_string(self.journal()).unwrap()
    }

    pub fn rewrite_first_line(&self, from: &str, to: &str) {
        let text = self.text();
        assert!(text.lines().next().unwrap().contains(from), "{from}");
        let first_end = text.find('\n').unwrap();
        let (first, rest) = text.split_at(first_end);
        std::fs::write(
            self.journal(),
            format!("{}{rest}", first.replacen(from, to, 1)),
        )
        .unwrap();
    }

    pub fn kinds(&self) -> Vec<String> {
        self.text()
            .lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                value["kind"].as_str().unwrap().to_owned()
            })
            .collect()
    }
}

pub fn files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, at: &Path, names: &mut Vec<String>) {
        for entry in std::fs::read_dir(at).unwrap().flatten() {
            let path = entry.path();
            names.push(path.strip_prefix(root).unwrap().display().to_string());
            if path.is_dir() {
                walk(root, &path, names);
            }
        }
    }
    let mut names = Vec::new();
    walk(dir, dir, &mut names);
    names.sort();
    names
}
