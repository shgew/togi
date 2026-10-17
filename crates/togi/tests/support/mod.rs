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
use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tempfile::TempDir;
use togi_cli::Exit;
use togi_cli::dashboard::Surface;
use togi_cli::system::System;
use togi_config::Config;
use togi_hardware::{Diagnosis, RebootError};
use togi_hostlock::{Error as LockError, HostLock, MemHostLocks, Policy};
use togi_journal::fs::Fs;
use togi_journal::{MemFs, Payload};
use togi_machine::{BiosContext, BootId, Machine};
use togi_render::OutputFacts;
use togi_tuningboot::{Bootloader, Environment, SavedEntry};
use togi_watchtest::{Scenario, install_with_fs};

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

#[expect(
    clippy::struct_excessive_bools,
    reason = "the host fake independently controls external checks"
)]
pub struct Host {
    pub linux: bool,
    pub boot_fails: bool,
    pub root: bool,
    pub fs: Arc<MemFs>,
    pub locks: MemHostLocks,
    pub lock_acquisitions: Cell<usize>,
    /// Why the next claim on the host lock fails, once.
    pub lock_error: RefCell<Option<LockError>>,
    pub real_fs: bool,
    pub lock: PathBuf,
    pub config: PathBuf,
    pub boot: String,
    pub steps: Steps,
    pub grubenvs: Rc<RefCell<Vec<PathBuf>>>,
    pub count: RefCell<Option<String>>,
    pub fail: RefCell<Vec<&'static str>>,
    pub marker: Cell<bool>,
    pub marker_at_reboot: Cell<Option<bool>>,
    pub reboot_error: RefCell<Option<RebootError>>,
    pub diagnosis: RefCell<Option<Result<Diagnosis, togi_hardware::Error>>>,
    pub diagnoses: RefCell<Vec<(Option<BiosContext>, bool)>>,
    pub calls: Cell<usize>,
    /// Whether a second claim on the host lock was refused as busy while `diagnose` ran; `None`
    /// when it did not run as root.
    pub lock_busy_in_diagnose: Cell<Option<bool>>,
    /// What `open_machine` hands out once: the machine of a command that opens one, or why it
    /// cannot. Empty when the test expects the command not to open any.
    pub machine: RefCell<Option<Result<Machine<'static>, togi_hardware::Error>>>,
    pub facts: OutputFacts,
    pub stdout_facts: Option<OutputFacts>,
    directory: Option<TempDir>,
}

impl Host {
    pub fn new() -> Self {
        Self::with_fs(Arc::new(MemFs::new()))
    }

    pub fn with_fs(fs: Arc<MemFs>) -> Self {
        Self {
            linux: true,
            boot_fails: false,
            root: true,
            fs,
            locks: MemHostLocks::default(),
            lock_acquisitions: Cell::new(0),
            lock_error: RefCell::new(None),
            real_fs: false,
            lock: PathBuf::from("/host/togi.lock"),
            config: PathBuf::from("/host/config.json"),
            boot: "0123456789abcdef0123456789abcdef".into(),
            steps: Steps::default(),
            grubenvs: Rc::default(),
            count: RefCell::new(None),
            fail: RefCell::default(),
            marker: Cell::new(false),
            marker_at_reboot: Cell::new(None),
            reboot_error: RefCell::new(None),
            diagnosis: RefCell::new(None),
            diagnoses: RefCell::default(),
            calls: Cell::new(0),
            lock_busy_in_diagnose: Cell::new(None),
            machine: RefCell::new(None),
            facts: OutputFacts::default(),
            stdout_facts: None,
            directory: None,
        }
    }

    pub fn real() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut host = Self::new();
        host.lock = dir.path().join("togi.lock");
        host.config = dir.path().join("config.json");
        host.real_fs = true;
        host.directory = Some(dir);
        host
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "fixture writes share the fallible filesystem interface"
    )]
    pub fn write(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.fs.put(path, bytes.as_ref().to_vec());
        Ok(())
    }

    pub fn read_to_string(&self, path: impl AsRef<Path>) -> io::Result<String> {
        String::from_utf8(self.fs.read(path.as_ref())?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn exists(&self, path: impl AsRef<Path>) -> bool {
        if self.real_fs {
            path.as_ref().exists()
        } else {
            self.fs.kind(path.as_ref()).is_ok_and(|kind| kind.is_some())
        }
    }

    pub fn create_dir_all(&self, path: impl AsRef<Path>) -> io::Result<()> {
        for ancestor in path
            .as_ref()
            .ancestors()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if ancestor.as_os_str().is_empty() || self.fs.kind(ancestor)?.is_some() {
                continue;
            }
            self.fs.create_dir(ancestor)?;
        }
        Ok(())
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

    fn acquire_host_lock(&self) -> Result<HostLock, LockError> {
        self.lock_acquisitions.set(self.lock_acquisitions.get() + 1);
        if let Some(error) = self.lock_error.borrow_mut().take() {
            return Err(error);
        }
        if self.real_fs {
            HostLock::acquire(&self.lock, Policy::Private)
        } else {
            self.locks.acquire(&self.lock)
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "explicit real filesystem command fixtures must never sync"
    )]
    fn journal_fs(&self) -> Option<Arc<dyn Fs>> {
        Some(if self.real_fs {
            togi_journal::fs::unsynced_disk_fs()
        } else {
            self.fs.clone()
        })
    }

    fn listen(
        &self,
        _: &togi_machine::CancellationToken,
    ) -> Result<Box<dyn std::any::Any>, togi_cli::signals::Error> {
        Ok(Box::new(()))
    }

    fn stdout_facts(&self) -> OutputFacts {
        self.stdout_facts.unwrap_or(self.facts)
    }

    fn config_path(&self) -> &Path {
        &self.config
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
        lock: Option<&HostLock>,
    ) -> Result<Diagnosis, togi_hardware::Error> {
        let privileged = lock.is_some();
        self.calls.set(self.calls.get() + 1);
        self.diagnoses
            .borrow_mut()
            .push((recorded.cloned(), privileged));
        if privileged {
            let busy = matches!(self.acquire_host_lock(), Err(LockError::Busy { .. }));
            self.lock_busy_in_diagnose.set(Some(busy));
        }
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
        if let Some(error) = self.reboot_error.borrow_mut().take() {
            return Err(error);
        }
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

    fn open_machine<'lock>(
        &self,
        _: &Config,
        _: &Path,
        _: &'lock HostLock,
    ) -> Result<Machine<'lock>, togi_hardware::Error> {
        self.steps.borrow_mut().push("open machine".into());
        self.machine
            .borrow_mut()
            .take()
            .expect("only run and emergency-stop open the machine, once, and the test stages it")
    }

    fn interactive(&self) -> bool {
        panic!("only run asks whether the terminal is interactive")
    }

    fn stderr(&self) -> Box<dyn Write> {
        panic!("only run keeps a handle on stderr")
    }

    fn surface(&self) -> Arc<dyn Surface> {
        panic!("only run draws the dashboard")
    }
}

/// A state directory holding the simulated journal of a session at its first search trial.
pub struct Session {
    pub dir: MemoryDir,
    pub fs: Arc<MemFs>,
    pub context: BiosContext,
}

impl Session {
    pub fn new() -> Self {
        Self::of(Scenario::Confirm)
    }

    pub fn of(scenario: Scenario) -> Self {
        Self::with_fs(Arc::new(MemFs::new()), scenario)
    }

    pub fn with_fs(fs: Arc<MemFs>, scenario: Scenario) -> Self {
        let dir = MemoryDir(PathBuf::from(format!("/state-{}", fs.entries().len())));
        install_with_fs(fs.clone(), dir.path(), scenario).unwrap();
        let context = scenario
            .events()
            .unwrap()
            .iter()
            .find_map(|event| match &event.payload {
                Payload::SessionContext(context) => Some(context.bios_context()),
                _ => None,
            })
            .expect("the scenario records a BIOS context");
        Self { dir, fs, context }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn host(&self) -> Host {
        Host::with_fs(self.fs.clone())
    }

    pub fn journal(&self) -> PathBuf {
        self.path().join("events.jsonl")
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.fs.contents(self.journal()).unwrap()).unwrap()
    }

    pub fn rewrite_first_line(&self, from: &str, to: &str) {
        let text = self.text();
        assert!(text.lines().next().unwrap().contains(from), "{from}");
        let first_end = text.find('\n').unwrap();
        let (first, rest) = text.split_at(first_end);
        self.fs.put(
            self.journal(),
            format!("{}{rest}", first.replacen(from, to, 1)).into_bytes(),
        );
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

pub struct MemoryDir(pub PathBuf);

impl MemoryDir {
    pub fn new() -> Self {
        Self(PathBuf::from("/fixture"))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

pub fn files(fs: &MemFs, dir: &Path) -> Vec<String> {
    fs.entries()
        .into_iter()
        .filter_map(|(path, _)| {
            let relative = path.strip_prefix(dir).ok()?;
            (!relative.as_os_str().is_empty()).then(|| relative.display().to_string())
        })
        .collect()
}
