//! A host for `togi run` made of fakes: a simulated machine, memory files and locks, an in-memory boot
//! entry and captured stderr. Explicit subprocess fixtures use unsynced disk storage.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_cli::dashboard::{Stop, Surface};
use togi_cli::system::System;
use togi_config::Config;
use togi_hardware::{Diagnosis, Error, RebootError};
use togi_hostlock::{HostLock, Policy};
use togi_journal::MemFs;
use togi_journal::fs::Fs;
use togi_machine::{
    BiosContext, BootId, CancellationToken, CoreId, Error as MachineError, Machine, ObservedOffset,
    Partial, Running, Signal, Smu, SmuValidated, Stopped, Swept, TargetOffset, TrialConditions,
    TrialSpec, Trials,
};
use togi_render::OutputFacts;
use togi_sim::definition::{CoreDef, ModelDef, Weights};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_tuningboot::night::{SETUP_PATH, Setup};
use togi_tuningboot::{Bootloader, Environment, SavedEntry};
use togi_watch::terminal;

const CONFIG: &str = "config.json";

/// Cores of the machine every `togi run` test drives.
pub(crate) const CORES: i64 = 4;

/// A four-core machine, two cores on each of two CCDs, whose cores all hold to CO -30 alone and together and fail
/// only past it: its stock controls never fail, so a run reaches the night's first written profile in a few trials.
pub(crate) fn simulator(seed: u64) -> Simulator {
    let core = (0..CORES)
        .map(|id| CoreDef {
            id,
            alone: [-30; 5],
            together: [-30; 7],
            flat: 0.0,
            idle: None,
            workload: BTreeMap::new(),
        })
        .collect();
    let definition = MachineDefinition {
        cores: CORES,
        core,
        model: ModelDef {
            past_limit_rate: 1.0,
            signals: Weights::from([(Signal::Crash, 1.0)]),
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    };
    Simulator::new(
        Model::new(&definition, seed).unwrap(),
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    )
    .unwrap()
}

/// Whether `spec` runs a core at an offset the run wrote: a night's stock controls run at the all-zero baseline, so
/// a run stopped during one has nothing to restore.
pub(crate) fn written(spec: &TrialSpec) -> bool {
    spec.offsets
        .iter()
        .any(|(_, offset)| offset != TargetOffset::ZERO)
}

/// What a fake writes to stderr, readable by the test.
#[derive(Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    pub(crate) fn text(&self) -> String {
        String::from_utf8(
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
        .unwrap()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// GRUB's environment in memory.
#[derive(Clone, Default)]
pub(crate) struct Grub(Arc<Mutex<GrubState>>);

#[derive(Default)]
pub(crate) struct GrubState {
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) cleared: u32,
    pub(crate) fail_set: bool,
    pub(crate) fail_clear: bool,
    pub(crate) log: Vec<&'static str>,
}

impl Grub {
    pub(crate) fn with_saved_entry(entry: &str) -> Self {
        let grub = Self::default();
        grub.state()
            .values
            .insert("saved_entry".to_owned(), entry.to_owned());
        grub
    }

    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, GrubState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Environment for Grub {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        Ok(self.state().values.get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        self.state().log.push("set");
        if self.state().fail_set {
            return Err(io::Error::other("environment block is read-only"));
        }
        for (name, value) in values {
            self.state()
                .values
                .insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        for name in names {
            self.state().values.remove(*name);
        }
        Ok(())
    }
}

impl Bootloader for Grub {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        let mut state = self.state();
        state.log.push("clear");
        state.cleared += 1;
        if state.fail_clear {
            return Err(io::Error::other("grub-editenv failed"));
        }
        let before = state.values.remove("saved_entry").unwrap_or_default();
        Ok(SavedEntry {
            before,
            after: String::new(),
        })
    }
}

/// A dashboard that does what the test says.
pub(crate) struct Scripted<F>(pub F);

impl<F: Fn(&Stop) -> Result<(), terminal::Error> + Send + Sync> Surface for Scripted<F> {
    fn draw(&self, _: &Path, _: u64, stop: &Stop) -> Result<(), terminal::Error> {
        (self.0)(stop)
    }
}

type WrapMachine = dyn Fn(Machine<'static>, &CancellationToken) -> Machine<'static>;

#[expect(
    clippy::struct_excessive_bools,
    reason = "a switchboard of independent fakes"
)]
pub(crate) struct Fake {
    pub(crate) sim: Simulator,
    pub(crate) dir: PathBuf,
    pub(crate) panic_in_platform: bool,
    pub(crate) policy: Policy,
    pub(crate) open_error: bool,
    pub(crate) real_stderr: bool,
    pub(crate) fs: Arc<dyn Fs>,
    pub(crate) locks: togi_hostlock::MemHostLocks,
    real_host: bool,
    cancel: RefCell<Option<CancellationToken>>,
    pub(crate) lock: PathBuf,
    /// The default configuration file: absent unless a test writes it.
    pub(crate) default_config: PathBuf,
    pub(crate) supported: bool,
    pub(crate) interactive: bool,
    /// What the host says about its stderr: terminal, colour, journald.
    pub(crate) facts: OutputFacts,
    /// Whether `systemctl reboot` fails.
    pub(crate) fail_reboot: bool,
    pub(crate) stderr: Capture,
    pub(crate) grub: Grub,
    pub(crate) surface: Arc<dyn Surface>,
    pub(crate) wrap: Box<WrapMachine>,
    pub(crate) opened: Cell<u32>,
    pub(crate) boots_read: Cell<u32>,
    pub(crate) reboots: Cell<u32>,
    /// Whether the host lock and the journal's lock were free when the reboot was requested.
    pub(crate) free_at_reboot: Cell<Option<(bool, bool)>>,
    /// Run once when the host lock is asked for, which is after the run's first, lock-free read of its journal and
    /// before its second, under both locks.
    pub(crate) at_host_lock: RefCell<Option<Box<dyn FnOnce()>>>,
    pub(crate) calls: RefCell<Vec<&'static str>>,
    /// Why the next claim on the host lock fails, once.
    pub(crate) lock_error: RefCell<Option<togi_hostlock::Error>>,
    /// Why the signal listener does not start, once.
    pub(crate) listen_error: RefCell<Option<togi_cli::signals::Error>>,
}

impl Fake {
    pub(crate) fn new() -> Self {
        Self::on(simulator(1000))
    }

    pub(crate) fn on(sim: Simulator) -> Self {
        Self::with_fs(sim, Path::new("/run-test"), Arc::new(MemFs::new()), false)
    }

    /// A genuine subprocess fixture: real locks, signals and files, but never disk synchronization.
    #[expect(
        clippy::disallowed_methods,
        reason = "real process fixtures must not sync to disk"
    )]
    pub(crate) fn in_dir(sim: Simulator, dir: &Path) -> Self {
        Self::with_fs(sim, dir, togi_journal::fs::unsynced_disk_fs(), true)
    }

    pub(crate) fn resume(&self) -> Self {
        let mut resumed = Self::with_fs(
            self.sim.clone(),
            &self.dir,
            Arc::clone(&self.fs),
            self.real_host,
        );
        resumed.locks = self.locks.clone();
        resumed
    }

    fn with_fs(sim: Simulator, dir: &Path, fs: Arc<dyn Fs>, real_host: bool) -> Self {
        if fs.kind(dir).unwrap().is_none() {
            fs.create_dir(dir).unwrap();
        }
        fs.create_truncate(&dir.join(CONFIG))
            .unwrap()
            .write_all(b"{}")
            .unwrap();
        Self {
            sim,
            lock: dir.join("togi.lock"),
            default_config: dir.join("default-config.json"),
            dir: dir.to_owned(),
            panic_in_platform: false,
            policy: Policy::Private,
            open_error: false,
            real_stderr: false,
            fs,
            locks: togi_hostlock::MemHostLocks::default(),
            real_host,
            cancel: RefCell::default(),
            supported: true,
            interactive: false,
            facts: OutputFacts::default(),
            fail_reboot: false,
            stderr: Capture::default(),
            grub: Grub::default(),
            surface: Arc::new(Scripted(|stop: &Stop| {
                stop.wait();
                Ok(())
            })),
            wrap: Box::new(|machine, _| machine),
            opened: Cell::new(0),
            boots_read: Cell::new(0),
            reboots: Cell::new(0),
            free_at_reboot: Cell::new(None),
            at_host_lock: RefCell::default(),
            calls: RefCell::default(),
            lock_error: RefCell::default(),
            listen_error: RefCell::default(),
        }
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.dir.join("state")
    }

    pub(crate) fn grubenv(&self) -> PathBuf {
        self.dir.join("grubenv")
    }

    /// `togi --state-dir <state> [--config <config>] <args>`: unless `args` name a configuration, the empty one in this
    /// host's directory, so no test reads the machine's own `/etc/togi/config.json`.
    pub(crate) fn arguments(&self, args: &[&str]) -> Vec<String> {
        let mut all = vec![
            "togi".to_owned(),
            "--state-dir".to_owned(),
            self.state_dir().display().to_string(),
        ];
        if !args.iter().any(|arg| arg.starts_with("--config")) {
            all.push("--config".to_owned());
            all.push(self.dir.join(CONFIG).display().to_string());
        }
        all.extend(args.iter().map(|arg| (*arg).to_owned()));
        all
    }

    /// Runs `togi <args>` against this host and returns its status, stdout and stderr.
    pub(crate) fn run(&self, args: &[&str]) -> (togi_cli::Exit, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = togi_cli::run_with(self.arguments(args), &mut out, &mut err, self);
        let mut stderr = String::from_utf8(err).unwrap();
        stderr.push_str(&self.stderr.text());
        (exit, String::from_utf8(out).unwrap(), stderr)
    }

    /// `togi --state-dir <state> <args>` with no `--config`, so the host's default configuration
    /// file ([`System::config_path`]) is the one read.
    pub(crate) fn run_on_default_config(&self, args: &[&str]) -> (togi_cli::Exit, String) {
        let mut all = vec![
            "togi".to_owned(),
            "--state-dir".to_owned(),
            self.state_dir().display().to_string(),
        ];
        all.extend(args.iter().map(|arg| (*arg).to_owned()));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = togi_cli::run_with(all, &mut out, &mut err, self);
        let mut stderr = String::from_utf8(err).unwrap();
        stderr.push_str(&self.stderr.text());
        (exit, stderr)
    }

    pub(crate) fn events(&self) -> Vec<togi_journal::Event> {
        togi_journal::Recording::read_in(&*self.fs, &self.state_dir().join("events.jsonl"))
            .map(togi_journal::Recording::into_events)
            .unwrap_or_default()
    }

    pub(crate) fn write(
        &self,
        path: impl AsRef<Path>,
        content: impl AsRef<[u8]>,
    ) -> io::Result<()> {
        self.fs
            .create_truncate(path.as_ref())?
            .write_all(content.as_ref())
    }

    pub(crate) fn read(&self, path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        self.fs.read(path.as_ref())
    }

    pub(crate) fn read_to_string(&self, path: impl AsRef<Path>) -> io::Result<String> {
        String::from_utf8(self.read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub(crate) fn create_dir(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.fs.create_dir(path.as_ref())
    }

    pub(crate) fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.fs.kind(path.as_ref()).unwrap().is_some()
    }

    pub(crate) fn hold_lock(&self) -> HostLock {
        if self.real_host {
            HostLock::acquire(&self.lock, self.policy).unwrap()
        } else {
            self.locks.acquire(&self.lock).unwrap()
        }
    }
}

impl System for Fake {
    fn check_platform(&self) -> Result<(), Error> {
        self.calls.borrow_mut().push("check_platform");
        assert!(
            !self.panic_in_platform,
            "injected panic before the host lock\x1b[31m"
        );
        if self.supported {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    }

    fn topology(&self) -> Result<Vec<togi_machine::CoreInfo>, Error> {
        panic!("run obtains topology from its machine, not the doctor survey")
    }

    fn config_path(&self) -> &Path {
        &self.default_config
    }

    fn journal_fs(&self) -> Option<Arc<dyn Fs>> {
        Some(Arc::clone(&self.fs))
    }

    fn acquire_host_lock(&self) -> Result<HostLock, togi_hostlock::Error> {
        let (path, policy) = self.host_lock();
        if let Some(error) = self.lock_error.borrow_mut().take() {
            return Err(error);
        }
        if self.real_host {
            HostLock::acquire(path, policy)
        } else if policy == Policy::Host {
            Err(togi_hostlock::Error::NotProvisioned {
                path: path.to_owned(),
            })
        } else {
            self.locks.acquire(path)
        }
    }

    fn listen(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn std::any::Any>, togi_cli::signals::Error> {
        *self.cancel.borrow_mut() = Some(cancel.clone());
        if let Some(error) = self.listen_error.borrow_mut().take() {
            return Err(error);
        }
        if self.real_host {
            togi_cli::system::listen_for_signals(cancel)
        } else {
            Ok(Box::new(()))
        }
    }

    fn host_lock(&self) -> (&Path, Policy) {
        self.calls.borrow_mut().push("host_lock");
        let change = self.at_host_lock.borrow_mut().take();
        if let Some(change) = change {
            change();
        }
        (&self.lock, self.policy)
    }

    fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
        self.calls.borrow_mut().push("boot_id");
        self.boots_read.set(self.boots_read.get() + 1);
        Ok(self.sim.seams().host.boot_id().unwrap())
    }

    fn now(&self) -> Timestamp {
        self.sim.now()
    }

    /// Like the real host's: the night setup file is read and decoded before a loader opens, so an unreadable or
    /// malformed one fails here; any other selection is the in-memory GRUB environment.
    fn bootloader(&self, selection: &Path) -> io::Result<Box<dyn Bootloader>> {
        if selection == Path::new(SETUP_PATH) {
            self.read_config(selection)?
                .parse::<Setup>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        Ok(Box::new(self.grub.clone()))
    }

    fn reboot(&self) -> Result<(), RebootError> {
        self.reboots.set(self.reboots.get() + 1);
        self.free_at_reboot.set(Some((
            if self.real_host {
                HostLock::acquire(&self.lock, self.policy).is_ok()
            } else {
                self.locks.acquire(&self.lock).is_ok()
            },
            self.lock_state_dir(&self.state_dir()).is_ok(),
        )));
        if self.fail_reboot {
            return Err(RebootError::Exit {
                status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
                stderr: "Failed to reboot".to_owned(),
            });
        }
        Ok(())
    }

    fn open_machine<'lock>(
        &self,
        _: &Config,
        _: &Path,
        _: &'lock HostLock,
    ) -> Result<Machine<'lock>, Error> {
        self.calls.borrow_mut().push("open_machine");
        self.opened.set(self.opened.get() + 1);
        if self.open_error {
            return Err(Error::NeedsRoot {
                operation: "open the SMU driver",
                uid: 1000,
            });
        }
        Ok((self.wrap)(
            self.sim.seams(),
            self.cancel.borrow().as_ref().unwrap(),
        ))
    }

    fn dashboard(&self, state: &Path, since: u64) -> togi_cli::dashboard::Dashboard {
        if self.real_host {
            togi_cli::dashboard::Dashboard::new(self.surface(), state, since)
        } else {
            let stderr = self.stderr.clone();
            togi_cli::dashboard::Dashboard::isolated(self.surface(), state, since, move |failure| {
                let _ = writeln!(
                    stderr.clone(),
                    "togi run: dashboard: {}; printing events instead",
                    togi_render::escape_text(&togi_render::error_chain(failure))
                );
            })
        }
    }

    fn interactive(&self) -> bool {
        self.interactive
    }

    fn stderr(&self) -> Box<dyn Write> {
        if self.real_stderr {
            return Box::new(io::stderr());
        }
        Box::new(self.stderr.clone())
    }

    fn stderr_facts(&self) -> OutputFacts {
        self.facts
    }
    fn stdout_facts(&self) -> OutputFacts {
        self.facts
    }

    fn stop_tuning(&self) -> Result<(), togi_hardware::StopError> {
        Err(togi_hardware::StopError::Exit {
            status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
            stderr: "test host has no tuning service".into(),
        })
    }

    fn answer(&self) -> io::Result<Option<String>> {
        Err(io::Error::other("test host has no terminal answer"))
    }

    fn prompt_line(&self) -> io::Result<Option<String>> {
        Err(io::Error::other("test host has no terminal input"))
    }

    fn day_observer<'lock>(
        &self,
        _: &'lock HostLock,
    ) -> Result<Box<dyn togi_hardware::day::Observer + 'lock>, MachineError> {
        Err(MachineError::Other("test host has no day observer".into()))
    }

    fn day_request(
        &self,
        _: &togi_cli::day::socket::Request,
    ) -> Result<togi_cli::day::socket::Reply, togi_cli::day::socket::Error> {
        Err(togi_cli::day::socket::Error::Io(io::Error::other(
            "test host has no day socket",
        )))
    }

    fn day_notification_actions(&self) -> bool {
        false
    }

    fn day_notification(
        &self,
        _: &togi_command::Invocation,
    ) -> Result<togi_command::Output, togi_command::Error> {
        Err(togi_command::Error::Spawn {
            program: "notify-send".into(),
            source: io::Error::other("test host has no notification daemon"),
        })
    }

    fn privileged(&self) -> bool {
        true
    }

    fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
        self.sim.seams().clock.monotonic()
    }

    fn zone(&self) -> TimeZone {
        TimeZone::UTC
    }

    fn diagnose(
        &self,
        _: &Config,
        _: Option<&BiosContext>,
        _: Option<&HostLock>,
    ) -> Result<Diagnosis, Error> {
        panic!("run does not diagnose")
    }

    fn diagnose_day(&self, _: &Config, _: Option<&HostLock>) -> Result<Diagnosis, Error> {
        panic!("run does not diagnose day prerequisites")
    }

    fn create_retry_marker(&self) -> io::Result<()> {
        panic!("run does not touch the retry marker")
    }

    fn remove_retry_marker(&self) -> io::Result<()> {
        panic!("run does not touch the retry marker")
    }

    fn surface(&self) -> Arc<dyn Surface> {
        Arc::clone(&self.surface)
    }
}

/// Cancels the run once the session writes its first offset, without sending a host signal.
struct InterruptingSmu {
    inner: Box<dyn Smu>,
    cancel: CancellationToken,
    sent: bool,
}

impl InterruptingSmu {
    fn interrupt(&mut self) {
        if !self.sent {
            self.sent = true;
            self.cancel.cancel();
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the fake injects cancellation before delegating simulated SMU writes"
)]
impl Smu for InterruptingSmu {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, MachineError> {
        self.inner.offset(core)
    }

    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), MachineError> {
        self.interrupt();
        self.inner.set_offset(core, offset)
    }

    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), MachineError> {
        self.interrupt();
        self.inner.set_all_offsets(offset)
    }

    fn emergency_zero(
        &mut self,
        validated: SmuValidated,
        stopped: &Stopped,
    ) -> Result<(), MachineError> {
        self.inner.emergency_zero(validated, stopped)
    }
}

/// The machine, cancelled at its first offset write.
pub(crate) fn interrupt_on_first_write(
    machine: Machine<'static>,
    cancel: &CancellationToken,
) -> Machine<'static> {
    Machine {
        smu: Box::new(InterruptingSmu {
            inner: machine.smu,
            cancel: cancel.clone(),
            sent: false,
        }),
        ..machine
    }
}

/// Cancels the run at its first trial, after any same-boot startup restoration.
struct InterruptingTrials {
    inner: Box<dyn Trials>,
    cancel: CancellationToken,
    sent: bool,
}

impl Trials for InterruptingTrials {
    fn controller_pid(&self) -> u32 {
        togi_sim::CONTROLLER_PID
    }

    fn content(
        &mut self,
        workload: togi_machine::WorkloadId,
    ) -> Result<togi_machine::ContentId, MachineError> {
        self.inner.content(workload)
    }

    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, MachineError> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        if !self.sent {
            self.sent = true;
            // Trial tokens are children: cancelling one alone does not stop the session.
            self.cancel.cancel();
        }
        self.inner.start(cancel, spec, ready)
    }

    fn ended(&mut self, id: &str) -> Result<(), MachineError> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, togi_machine::SampleError>> + 'a> {
        self.inner.samples(id)
    }
}

/// The machine, cancelled when its first trial starts.
pub(crate) fn interrupt_at_first_trial(
    machine: Machine<'static>,
    cancel: &CancellationToken,
) -> Machine<'static> {
    Machine {
        trials: Box::new(InterruptingTrials {
            inner: machine.trials,
            cancel: cancel.clone(),
            sent: false,
        }),
        ..machine
    }
}

/// A host whose identity never validates, counting how often the BIOS context is read anyway.
pub(crate) struct UnvalidatedHost {
    pub(crate) inner: Box<dyn togi_machine::Host>,
    pub(crate) bios_reads: Arc<std::sync::atomic::AtomicU32>,
}

impl togi_machine::Host for UnvalidatedHost {
    fn boot_id(&mut self) -> Result<BootId, MachineError> {
        self.inner.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<togi_machine::CoreInfo>, MachineError> {
        self.inner.topology()
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, MachineError> {
        Err(MachineError::Other("unsupported CPU model".into()))
    }

    fn bios_context(&mut self) -> Result<togi_machine::BiosContext, MachineError> {
        self.bios_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.bios_context()
    }

    fn ranking(
        &mut self,
    ) -> Result<Vec<togi_machine::CoreRank>, togi_machine::Partial<Vec<togi_machine::CoreRank>>>
    {
        self.inner.ranking()
    }

    fn preflight(&mut self) -> Vec<togi_machine::Check> {
        vec![togi_machine::Check {
            name: "cpu".to_owned(),
            detail: "unsupported CPU model".to_owned(),
            ok: false,
        }]
    }

    fn backends(&mut self) -> Vec<togi_machine::BackendCheck> {
        self.inner.backends()
    }

    fn watchdog(&mut self) -> togi_machine::Check {
        self.inner.watchdog()
    }
}

/// A host whose cores sit on four CCDs, a quarter of them each: one CCD more than the tuner supports.
pub(crate) struct FourCcds(pub(crate) Box<dyn togi_machine::Host>);

impl togi_machine::Host for FourCcds {
    fn boot_id(&mut self) -> Result<BootId, MachineError> {
        self.0.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<togi_machine::CoreInfo>, MachineError> {
        let mut cores = self.0.topology()?;
        let quarter = cores.len().div_ceil(4);
        for (index, info) in cores.iter_mut().enumerate() {
            info.ccd = togi_machine::CcdId::try_from(index / quarter).unwrap();
        }
        Ok(cores)
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, MachineError> {
        self.0.validate_smu()
    }

    fn bios_context(&mut self) -> Result<togi_machine::BiosContext, MachineError> {
        self.0.bios_context()
    }

    fn ranking(
        &mut self,
    ) -> Result<Vec<togi_machine::CoreRank>, togi_machine::Partial<Vec<togi_machine::CoreRank>>>
    {
        self.0.ranking()
    }

    fn preflight(&mut self) -> Vec<togi_machine::Check> {
        self.0.preflight()
    }

    fn backends(&mut self) -> Vec<togi_machine::BackendCheck> {
        self.0.backends()
    }

    fn watchdog(&mut self) -> togi_machine::Check {
        self.0.watchdog()
    }
}
