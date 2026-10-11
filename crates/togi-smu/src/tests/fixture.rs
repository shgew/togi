use crate::mailbox::{Command, MailboxError, Status, Transport};
use crate::{Driver, Error};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tempfile::TempDir;
use togi_machine::{CoreId, ObservedOffset, Smu, SmuValidated, Stopped, TargetOffset};

pub(super) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Blocks every caller of `wait` until `open`.
#[derive(Clone, Default)]
pub(super) struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    pub(super) fn wait(&self) {
        let (state, opened) = &*self.0;
        let mut open = lock(state);
        while !*open {
            open = opened.wait(open).unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub(super) fn open(&self) {
        let (state, opened) = &*self.0;
        *lock(state) = true;
        opened.notify_all();
    }
}

pub(super) const RSMU_RESPONSE: u32 = 0x03b1_0570;
pub(super) const RSMU_COMMAND: u32 = 0x03b1_0524;
pub(super) const CCD0_FUSE: u32 = 0x304a_03dc;
pub(super) const CCD1_FUSE: u32 = CCD0_FUSE + (1 << 25);
pub(super) const GENEROUS: Duration = Duration::from_secs(30);

pub(super) struct Tree {
    dir: TempDir,
}

impl Tree {
    pub(super) fn new(cores_per_ccd: usize, cache: bool) -> Self {
        Self::with_ccds(2, cores_per_ccd, cache)
    }

    pub(super) fn with_ccds(ccds: usize, cores_per_ccd: usize, cache: bool) -> Self {
        let tree = Self {
            dir: TempDir::new().unwrap(),
        };
        tree.put(
            "proc/cpuinfo",
            "cpu family : 26\nmodel : 68\nmodel name : Zen Test\n",
        );
        tree.put("sys/kernel/ryzen_smu_drv/codename", "23\n");
        tree.put("sys/kernel/ryzen_smu_drv/drv_version", "0.1\n");
        tree.put("sys/kernel/ryzen_smu_drv/version", "57.13\n");
        for ccd in 0..ccds {
            for index in 0..cores_per_ccd {
                let core = ccd * 8 + index;
                tree.cpu(core, core, ccd, cache);
                tree.cpu(core + 16, core, ccd, cache);
            }
        }
        tree
    }

    pub(super) fn root(&self) -> &Path {
        self.dir.path()
    }

    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    pub(super) fn put(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    pub(super) fn remove(&self, relative: &str) {
        fs::remove_file(self.path(relative)).unwrap();
    }

    pub(super) fn cpu(&self, cpu: usize, core: usize, ccd: usize, cache: bool) {
        let base = format!("sys/devices/system/cpu/cpu{cpu}");
        self.put(&format!("{base}/topology/core_id"), &core.to_string());
        self.put(&format!("{base}/topology/die_id"), &(ccd + 10).to_string());
        self.put(&format!("{base}/topology/physical_package_id"), "0");
        if cache {
            self.put(&format!("{base}/cache/index3/id"), &(ccd + 20).to_string());
        }
    }

    pub(super) fn identity_files(&self) {
        self.put("sys/class/dmi/id/bios_version", "3.14\n");
        self.put("sys/class/dmi/id/board_vendor", "ASRock\n");
        self.put("sys/class/dmi/id/board_name", "X870E Taichi\n");
    }
}

type SmnHook = Arc<Mutex<dyn FnMut(u32, u32) -> u32 + Send>>;
type CommandHook = Arc<dyn Fn() + Send + Sync>;

struct FakeState {
    values: HashMap<u32, u32>,
    responses: HashMap<Command, [u32; 6]>,
    commands: Vec<(Command, u32)>,
    smn: Vec<u32>,
    status: Option<Status>,
    failing_smn_reads: Vec<usize>,
    smn_hook: Option<SmnHook>,
    command_hook: Option<CommandHook>,
}

/// A counting mailbox: records every command and SMN read it receives and answers from tables.
#[derive(Clone)]
pub(super) struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    pub(super) fn new() -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            values: HashMap::from([(RSMU_RESPONSE, 1), (RSMU_COMMAND, 0x6e)]),
            responses: HashMap::new(),
            commands: Vec::new(),
            smn: Vec::new(),
            status: None,
            failing_smn_reads: Vec::new(),
            smn_hook: None,
            command_hook: None,
        })))
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn set_value(&self, address: u32, value: u32) {
        self.state().values.insert(address, value);
    }

    pub(super) fn respond(&self, command: Command, word: u32) {
        self.state()
            .responses
            .insert(command, [word, 0, 0, 0, 0, 0]);
    }

    pub(super) fn fail_commands(&self, status: Status) {
        self.state().status = Some(status);
    }

    pub(super) fn fail_smn_read(&self, index: usize) {
        self.state().failing_smn_reads.push(index);
    }

    pub(super) fn hook_smn(&self, hook: impl FnMut(u32, u32) -> u32 + Send + 'static) {
        self.state().smn_hook = Some(Arc::new(Mutex::new(hook)));
    }

    pub(super) fn hook_command(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.state().command_hook = Some(Arc::new(hook));
    }

    pub(super) fn commands(&self) -> Vec<(Command, u32)> {
        self.state().commands.clone()
    }

    pub(super) fn smn_reads(&self) -> Vec<u32> {
        self.state().smn.clone()
    }

    pub(super) fn hardware_calls(&self) -> usize {
        let state = self.state();
        state.commands.len() + state.smn.len()
    }
}

impl Transport for Fake {
    fn command(&mut self, command: Command, args: [u32; 6]) -> Result<[u32; 6], MailboxError> {
        let (hook, result) = {
            let mut state = self.state();
            state.commands.push((command, args[0]));
            let result = match state.status {
                Some(status) => Err(MailboxError::Status {
                    command: command.code(),
                    status,
                }),
                None => Ok(state.responses.get(&command).copied().unwrap_or_default()),
            };
            (state.command_hook.clone(), result)
        };
        if let Some(hook) = hook {
            hook();
        }
        result
    }

    fn read_smn(&mut self, address: u32) -> Result<u32, MailboxError> {
        let (value, hook) = {
            let mut state = self.state();
            state.smn.push(address);
            if state.failing_smn_reads.contains(&state.smn.len()) {
                return Err(MailboxError::Read {
                    path: PathBuf::from("smn"),
                    source: io::Error::other("SMN unavailable"),
                });
            }
            (
                state.values.get(&address).copied().unwrap_or_default(),
                state.smn_hook.clone(),
            )
        };
        Ok(match hook {
            Some(hook) => (*lock(&hook))(address, value),
            None => value,
        })
    }
}

pub(super) fn open(tree: &Tree, fake: &Fake) -> Driver {
    Driver::with_transport(tree.root(), Box::new(fake.clone()), GENEROUS).unwrap()
}

pub(super) fn open_error(tree: &Tree, fake: &Fake) -> Error {
    Driver::with_transport(tree.root(), Box::new(fake.clone()), GENEROUS).unwrap_err()
}

pub(super) fn validated(tree: &Tree, fake: &Fake) -> Driver {
    let mut driver = open(tree, fake);
    let _ = driver.validate().unwrap();
    driver
}

pub(super) fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

pub(super) fn read(driver: &mut Driver, id: i64) -> Result<ObservedOffset, togi_machine::Error> {
    driver.offset(core(id))
}

#[expect(
    clippy::disallowed_methods,
    reason = "negative control: only the session write transaction calls the seam's per-core write"
)]
pub(super) fn write(
    driver: &mut Driver,
    id: i64,
    target: TargetOffset,
) -> Result<(), togi_machine::Error> {
    driver.set_offset(core(id), target)
}

#[expect(
    clippy::disallowed_methods,
    reason = "negative control: only the session write transaction calls the seam's broadcast write"
)]
pub(super) fn write_all(
    driver: &mut Driver,
    target: TargetOffset,
) -> Result<(), togi_machine::Error> {
    driver.set_all_offsets(target)
}

#[expect(
    clippy::disallowed_methods,
    reason = "negative control: only the session's emergency path calls the seam's zeroing"
)]
pub(super) fn zero_all(
    driver: &mut Driver,
    proof: SmuValidated,
) -> Result<(), togi_machine::Error> {
    driver.emergency_zero(proof, &stopped())
}

#[expect(
    clippy::disallowed_methods,
    reason = "test mints a proof to show a driver that never validated still refuses"
)]
pub(super) fn forged_proof() -> SmuValidated {
    SmuValidated::checked()
}

pub(super) fn smu_error(error: &togi_machine::Error) -> &Error {
    match error {
        togi_machine::Error::Other(inner) => inner.downcast_ref::<Error>().unwrap(),
        other => panic!("not a togi-smu error: {other}"),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "test proof: no workload ever ran against the driver under test"
)]
pub(super) fn stopped() -> Stopped {
    Stopped::confirmed()
}

pub(super) fn chain(error: &dyn std::error::Error) -> String {
    crate::error::chain(error)
}

/// Every hardware-reaching entry point of a driver that must refuse; names those that did not.
pub(super) fn accepted_unvalidated(driver: &mut Driver) -> Vec<&'static str> {
    let target = TargetOffset::clamped(-10);
    let mut accepted = Vec::new();
    let mut note = |name, accepts: bool| {
        if accepts {
            accepted.push(name);
        }
    };
    note("offset", read(driver, 0).is_ok());
    note("set_offset", write(driver, 0, target).is_ok());
    note("set_all_offsets", write_all(driver, target).is_ok());
    note("emergency_zero", zero_all(driver, forged_proof()).is_ok());
    note("bios_context", driver.bios_context().is_ok());
    note("pm_reader", driver.pm_reader().is_ok());
    note("readback", driver.check_readback().ok);
    note("slot_mapping", driver.check_slot_mapping().ok);
    accepted
}
