//! A host for `togi run` made of fakes: a simulated machine, a lock and a state directory in a temporary
//! directory, an in-memory boot entry and captured stderr. Nothing here reaches real hardware.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use jiff::Timestamp;
use tempfile::TempDir;
use togi_cli::dashboard::{Stop, Surface};
use togi_cli::system::System;
use togi_config::Config;
use togi_hardware::{Error, RebootError};
use togi_hostlock::{HostLock, Policy};
use togi_machine::{
    BootId, CoreId, Error as MachineError, Machine, ObservedOffset, Smu, SmuValidated, Stopped,
    TargetOffset,
};
use togi_render::Renderer;
use togi_sim::{Model, RunSettings, Simulator, file};
use togi_tuningboot::{Bootloader, Environment, SavedEntry};

pub(crate) const MACHINE: &str = "shared-voltage";

pub(crate) fn simulator(seed: u64) -> Simulator {
    biased(seed, 0)
}

/// The machine with every core's boot offset at `bios`, which a run has to restore after it wrote others.
pub(crate) fn biased(seed: u64, bios: i64) -> Simulator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/bench/machines")
        .join(format!("{MACHINE}.json"));
    let mut definition = file::load(path).unwrap();
    definition.bios = (bios != 0).then(|| vec![bios; 16]);
    Simulator::new(
        Model::new(&definition, seed).unwrap(),
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    )
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

impl<F: Fn(&Stop) -> Result<(), String> + Send + Sync> Surface for Scripted<F> {
    fn draw(&self, _: &Path, _: u64, stop: &Stop) -> Result<(), String> {
        (self.0)(stop)
    }
}

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
    temp: Option<TempDir>,
    pub(crate) lock: PathBuf,
    pub(crate) supported: bool,
    pub(crate) interactive: bool,
    pub(crate) stderr: Capture,
    pub(crate) grub: Grub,
    pub(crate) surface: Arc<dyn Surface>,
    pub(crate) wrap: Box<dyn Fn(Machine) -> Machine>,
    pub(crate) opened: Cell<u32>,
    pub(crate) boots_read: Cell<u32>,
    pub(crate) reboots: Cell<u32>,
    pub(crate) calls: RefCell<Vec<&'static str>>,
}

impl Fake {
    pub(crate) fn new() -> Self {
        Self::on(simulator(1000))
    }

    pub(crate) fn on(sim: Simulator) -> Self {
        let temp = TempDir::new().unwrap();
        let mut fake = Self::in_dir(sim, temp.path());
        fake.temp = Some(temp);
        fake
    }

    /// A host whose files live in `dir`.
    pub(crate) fn in_dir(sim: Simulator, dir: &Path) -> Self {
        Self {
            sim,
            lock: dir.join("togi.lock"),
            dir: dir.to_owned(),
            panic_in_platform: false,
            policy: Policy::Private,
            open_error: false,
            real_stderr: false,
            temp: None,
            supported: true,
            interactive: false,
            stderr: Capture::default(),
            grub: Grub::default(),
            surface: Arc::new(Scripted(|stop: &Stop| {
                stop.wait();
                Ok(())
            })),
            wrap: Box::new(|machine| machine),
            opened: Cell::new(0),
            boots_read: Cell::new(0),
            reboots: Cell::new(0),
            calls: RefCell::default(),
        }
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.dir.join("state")
    }

    pub(crate) fn grubenv(&self) -> PathBuf {
        self.dir.join("grubenv")
    }

    pub(crate) fn arguments(&self, args: &[&str]) -> Vec<String> {
        let mut all = vec![
            "togi".to_owned(),
            "--state-dir".to_owned(),
            self.state_dir().display().to_string(),
        ];
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

    pub(crate) fn events(&self) -> Vec<togi_journal::Event> {
        togi_journal::Recording::read(&self.state_dir().join("events.jsonl"))
            .map(togi_journal::Recording::into_events)
            .unwrap_or_default()
    }
}

impl System for Fake {
    fn check_platform(&self) -> Result<(), Error> {
        self.calls.borrow_mut().push("check_platform");
        assert!(
            !self.panic_in_platform,
            "injected panic before the host lock"
        );
        if self.supported {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    }

    fn host_lock(&self) -> (&Path, Policy) {
        self.calls.borrow_mut().push("host_lock");
        (&self.lock, self.policy)
    }

    fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
        self.boots_read.set(self.boots_read.get() + 1);
        Ok(self.sim.seams().host.boot_id().unwrap())
    }

    fn now(&self) -> Timestamp {
        self.sim.now()
    }

    fn bootloader(&self, _: &Path) -> Box<dyn Bootloader> {
        Box::new(self.grub.clone())
    }

    fn reboot(&self) -> Result<(), RebootError> {
        self.reboots.set(self.reboots.get() + 1);
        Ok(())
    }

    fn open_machine(&self, _: &Config, _: &Path, _: &HostLock) -> Result<Machine, Error> {
        self.opened.set(self.opened.get() + 1);
        if self.open_error {
            return Err(Error::NeedsRoot {
                operation: "open the SMU driver",
                uid: 1000,
            });
        }
        Ok((self.wrap)(self.sim.seams()))
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

    fn renderer(&self) -> Renderer {
        Renderer::default()
    }

    fn surface(&self) -> Arc<dyn Surface> {
        Arc::clone(&self.surface)
    }
}

/// Sends the process SIGINT, as Ctrl-C does, once the session writes its first offset. Each test runs in its own
/// process (nextest), so the signal reaches only the run under test.
struct InterruptingSmu {
    inner: Box<dyn Smu>,
    sent: bool,
}

impl InterruptingSmu {
    fn interrupt(&mut self) {
        if !self.sent {
            self.sent = true;
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::INT)
                .unwrap();
        }
    }
}

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

/// The machine, interrupted by SIGINT at its first offset write.
pub(crate) fn interrupt_on_first_write(machine: Machine) -> Machine {
    Machine {
        smu: Box::new(InterruptingSmu {
            inner: machine.smu,
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

    fn watchdog(&mut self) -> togi_machine::Check {
        self.inner.watchdog()
    }
}
