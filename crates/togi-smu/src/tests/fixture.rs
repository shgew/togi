use crate::files::{Files, Readers};
use crate::mailbox::{Command, MailboxError, Status, Transport};
use crate::pm_table::{PmFile, PmSource};
#[cfg(target_os = "linux")]
use crate::pm_table::{TABLE_LEN, VERSION};
#[cfg(target_os = "linux")]
use crate::sysfs::DRIVER_DIR;
use crate::{Driver, Error};
#[cfg(target_os = "linux")]
use rustix::fd::OwnedFd;
#[cfg(target_os = "linux")]
use rustix::fs::inotify;
#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;
use std::io;
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
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

    /// Waits for `open`, giving up after `limit`; says whether the gate opened.
    pub(super) fn wait_timeout(&self, limit: Duration) -> bool {
        let (state, opened) = &*self.0;
        let (open, _) = opened
            .wait_timeout_while(lock(state), limit, |open| !*open)
            .unwrap_or_else(PoisonError::into_inner);
        *open
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
    dir: Option<TempDir>,
    files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
    errors: Arc<Mutex<HashMap<PathBuf, io::ErrorKind>>>,
}

impl Tree {
    pub(super) fn new(cores_per_ccd: usize, cache: bool) -> Self {
        Self::with_ccds(2, cores_per_ccd, cache)
    }

    pub(super) fn with_ccds(ccds: usize, cores_per_ccd: usize, cache: bool) -> Self {
        let tree = Self {
            dir: None,
            files: Arc::default(),
            errors: Arc::default(),
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
    #[cfg(target_os = "linux")]
    pub(super) fn real(cores_per_ccd: usize, cache: bool) -> Self {
        let memory = Self::new(cores_per_ccd, cache);
        let tree = Self {
            dir: Some(TempDir::new().unwrap()),
            files: Arc::default(),
            errors: Arc::default(),
        };
        for (path, bytes) in lock(&memory.files).iter() {
            let relative = path.strip_prefix(memory.root()).unwrap().to_str().unwrap();
            tree.put_bytes(relative, bytes);
        }
        tree
    }

    pub(super) fn source(&self) -> Files {
        if self.dir.is_some() {
            return Files::System;
        }
        let files = Arc::clone(&self.files);
        let errors = Arc::clone(&self.errors);
        let written = Arc::clone(&self.files);
        let base = self.path("sys/devices/system/cpu");
        let mut cpu_names: Vec<String> = lock(&files)
            .keys()
            .filter_map(|path| path.strip_prefix(&base).ok()?.components().next())
            .map(|name| name.as_os_str().to_str().unwrap().to_owned())
            .collect();
        cpu_names.sort();
        cpu_names.dedup();
        Files::Readers(Arc::new(Readers {
            cpu_names,
            read: Arc::new(move |path| {
                if let Some(kind) = lock(&errors).get(path) {
                    return Err((*kind).into());
                }
                lock(&files)
                    .get(path)
                    .cloned()
                    .ok_or_else(|| io::ErrorKind::NotFound.into())
            }),
            write: Arc::new(move |path, bytes| {
                let mut files = lock(&written);
                let existing = files
                    .get_mut(path)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
                if existing.len() < bytes.len() {
                    existing.resize(bytes.len(), 0);
                }
                existing[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }),
        }))
    }

    pub(super) fn open(&self) -> Result<Driver, Error> {
        let Files::Readers(readers) = self.source() else {
            return Driver::open(self.root());
        };
        let read = Arc::clone(&readers.read);
        let write = Arc::clone(&readers.write);
        let loaded = lock(&self.files)
            .keys()
            .any(|path| path.starts_with(self.path("sys/kernel/ryzen_smu_drv")));
        Driver::from_readers(
            self.root(),
            readers.cpu_names.clone(),
            loaded,
            move |path| read(path),
            move |path, bytes| write(path, bytes),
        )
    }

    pub(super) fn identity_only(&self) -> Driver {
        let Files::Readers(readers) = self.source() else {
            return Driver::identity_only(self.root()).unwrap();
        };
        let read = Arc::clone(&readers.read);
        Driver::identity_only_from_readers(
            self.root(),
            readers.cpu_names.clone(),
            true,
            move |path| read(path),
        )
        .unwrap()
    }

    pub(super) fn fail_read(&self, relative: &str, kind: io::ErrorKind) {
        lock(&self.errors).insert(self.path(relative), kind);
    }

    pub(super) fn remove_dir(&self, relative: &str) {
        let path = self.path(relative);
        lock(&self.files).retain(|file, _| !file.starts_with(&path));
    }

    pub(super) fn bytes(&self, relative: &str) -> Vec<u8> {
        match self.source() {
            Files::System => fs::read(self.path(relative)).unwrap(),
            Files::Readers(readers) => (readers.read)(&self.path(relative)).unwrap(),
        }
    }

    pub(super) fn root(&self) -> &Path {
        self.dir
            .as_ref()
            .map_or(Path::new("/fixture"), TempDir::path)
    }

    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    pub(super) fn put(&self, relative: &str, text: &str) {
        self.put_bytes(relative, text.as_bytes());
    }

    pub(super) fn remove(&self, relative: &str) {
        if self.dir.is_some() {
            fs::remove_file(self.path(relative)).unwrap();
        } else {
            assert!(lock(&self.files).remove(&self.path(relative)).is_some());
        }
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

    pub(super) fn put_bytes(&self, relative: &str, bytes: &[u8]) {
        let path = self.path(relative);
        if self.dir.is_some() {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        } else {
            lock(&self.files).insert(path, bytes.to_vec());
        }
    }

    /// The mailbox and PM table files, answering as a healthy `ryzen_smu` driver would.
    #[cfg(target_os = "linux")]
    pub(super) fn hardware_files(&self) {
        let driver = |name: &str| format!("{DRIVER_DIR}/{name}");
        self.put_bytes(&driver("smu_args"), &[0; 24]);
        self.put_bytes(&driver("rsmu_cmd"), &1_u32.to_le_bytes());
        self.put_bytes(&driver("smn"), &[0; 4]);
        self.put_bytes(&driver("pm_table_version"), &VERSION.to_le_bytes());
        self.put_bytes(&driver("pm_table"), &[0; TABLE_LEN]);
    }
}

/// The `ryzen_smu` files whose opening means hardware was reached: the mailbox and the PM table.
#[cfg(target_os = "linux")]
pub(super) const HARDWARE_FILES: [&str; 5] = [
    "smu_args",
    "rsmu_cmd",
    "smn",
    "pm_table_version",
    "pm_table",
];

/// An inotify watch on every hardware file of a `Tree`: any open, read, write or attribute
/// change of one is an event, so a driver that reached the real files cannot go unnoticed.
#[cfg(target_os = "linux")]
pub(super) struct Watch {
    inotify: OwnedFd,
    files: Vec<(i32, &'static str)>,
}

#[cfg(target_os = "linux")]
impl Watch {
    pub(super) fn hardware_files(tree: &Tree) -> Self {
        let inotify =
            inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC).unwrap();
        let files = HARDWARE_FILES
            .into_iter()
            .map(|name| {
                let path = tree.path(&format!("{DRIVER_DIR}/{name}"));
                let watch = inotify::add_watch(&inotify, path, inotify::WatchFlags::ALL_EVENTS);
                (watch.unwrap(), name)
            })
            .collect();
        Self { inotify, files }
    }

    /// The watched files with an event since the last call, sorted.
    pub(super) fn touched(&self) -> Vec<&'static str> {
        let mut buffer = [MaybeUninit::uninit(); 1024];
        let mut reader = inotify::Reader::new(&self.inotify, &mut buffer);
        let mut touched = BTreeSet::new();
        loop {
            match reader.next() {
                Err(Errno::AGAIN) => break,
                Err(error) => panic!("reading the inotify queue: {error}"),
                Ok(event) => {
                    let file = self.files.iter().find(|(wd, _)| *wd == event.wd());
                    touched.insert(file.map_or("an unknown watch", |(_, name)| *name));
                }
            }
        }
        touched.into_iter().collect()
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
    pm: Vec<PmFile>,
    pm_version: Option<Vec<u8>>,
    pm_table: Option<Vec<u8>>,
}

/// A counting hardware stand-in: records every command, SMN read and PM file read it receives and
/// answers from tables.
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
            pm: Vec::new(),
            pm_version: None,
            pm_table: None,
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
        state.commands.len() + state.smn.len() + state.pm.len()
    }

    /// How many times the PM source was asked for `file`.
    pub(super) fn pm_reads(&self, file: PmFile) -> usize {
        self.state().pm.iter().filter(|read| **read == file).count()
    }

    /// What the PM files hold; `None` makes the read fail like a missing file.
    pub(super) fn serve_pm(&self, version: Option<&[u8]>, table: Option<&[u8]>) {
        let mut state = self.state();
        state.pm_version = version.map(<[u8]>::to_vec);
        state.pm_table = table.map(<[u8]>::to_vec);
    }
}

impl PmSource for Fake {
    fn read(&self, file: PmFile) -> io::Result<Vec<u8>> {
        let mut state = self.state();
        state.pm.push(file);
        let content = match file {
            PmFile::Version => &state.pm_version,
            PmFile::Table => &state.pm_table,
        };
        content
            .clone()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
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
    open_within(tree, fake, GENEROUS)
}

/// Like `open`, with the given mailbox deadline from the first transaction on.
pub(super) fn open_within(tree: &Tree, fake: &Fake, deadline: Duration) -> Driver {
    Driver::with_transport(
        tree.root(),
        tree.source(),
        Box::new(fake.clone()),
        Arc::new(fake.clone()),
        deadline,
    )
    .unwrap()
}

pub(super) fn open_error(tree: &Tree, fake: &Fake) -> Error {
    Driver::with_transport(
        tree.root(),
        tree.source(),
        Box::new(fake.clone()),
        Arc::new(fake.clone()),
        GENEROUS,
    )
    .unwrap_err()
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
    let togi_machine::Error::Other(inner) = error else {
        panic!("not a togi-smu error: {error}");
    };
    inner.downcast_ref::<Error>().unwrap()
}

#[expect(
    clippy::disallowed_methods,
    reason = "test proof: no workload ever ran against the driver under test"
)]
pub(super) fn stopped() -> Stopped {
    Stopped::confirmed()
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
