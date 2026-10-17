#[cfg(target_os = "linux")]
use std::cell::Cell;
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use togi_config::Config;
use togi_machine::{Check, CoreInfo, CoreRank, Partial};
#[cfg(target_os = "linux")]
use togi_smu::Driver;
#[cfg(target_os = "linux")]
use togi_trial::{BackendIdentity, IdentityError};

use crate::adapters::lock;
#[cfg(target_os = "linux")]
use crate::assemble::Parts;
use crate::host::HostReadings;
#[cfg(target_os = "linux")]
use crate::host::{Privilege, ScopeProbe, SystemHost};
#[cfg(target_os = "linux")]
use crate::preflight::Account;

pub(super) const MAILBOX_AND_PM_FILES: [&str; 5] = [
    "smu_args",
    "rsmu_cmd",
    "smn",
    "pm_table_version",
    "pm_table",
];

#[derive(Clone)]
enum Entry {
    Directory,
    File(Vec<u8>),
}

#[derive(Clone)]
pub(super) struct Tree {
    entries: Arc<Mutex<BTreeMap<PathBuf, Entry>>>,
    touched: Arc<Mutex<Vec<&'static str>>>,
}

impl Tree {
    pub(super) fn empty() -> Self {
        Self {
            entries: Arc::default(),
            touched: Arc::default(),
        }
    }

    #[cfg(target_os = "linux")]
    /// A supported identity with one physical core: mapping must refuse before SMN access.
    pub(super) fn new() -> Self {
        let tree = Self::empty();
        tree.put(
            "proc/cpuinfo",
            "cpu family : 26\nmodel : 68\nmodel name : Test CPU\nmicrocode : 0xb404038\n",
        );
        tree.put("sys/kernel/ryzen_smu_drv/codename", "23\n");
        tree.put("sys/kernel/ryzen_smu_drv/drv_version", "0.1\n");
        tree.put("sys/kernel/ryzen_smu_drv/version", "57.13\n");
        for cpu in [0, 16] {
            let base = format!("sys/devices/system/cpu/cpu{cpu}/topology");
            tree.put(&format!("{base}/core_id"), "0");
            tree.put(&format!("{base}/die_id"), "10");
            tree.put(&format!("{base}/physical_package_id"), "0");
        }
        for name in MAILBOX_AND_PM_FILES {
            tree.put(&format!("sys/kernel/ryzen_smu_drv/{name}"), "");
        }
        tree
    }

    #[expect(
        clippy::unused_self,
        reason = "the memory fixture has a fixed virtual root"
    )]
    pub(super) fn root(&self) -> &Path {
        Path::new("/fixture")
    }

    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    pub(super) fn mkdir(&self, relative: &str) {
        let path = self.path(relative);
        let mut entries = lock(&self.entries);
        for parent in path.ancestors() {
            entries.entry(parent.to_owned()).or_insert(Entry::Directory);
        }
    }

    pub(super) fn put(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        let parent = path.parent().unwrap().strip_prefix(self.root()).unwrap();
        self.mkdir(parent.to_str().unwrap());
        lock(&self.entries).insert(path, Entry::File(text.as_bytes().to_vec()));
    }

    #[cfg(target_os = "linux")]
    pub(super) fn remove(&self, relative: &str) {
        assert!(lock(&self.entries).remove(&self.path(relative)).is_some());
    }

    #[cfg(target_os = "linux")]
    pub(super) fn remove_dir(&self, relative: &str) {
        let path = self.path(relative);
        lock(&self.entries).retain(|entry, _| !entry.starts_with(&path));
    }

    fn record(&self, path: &Path) {
        if path.parent() == Some(self.path("sys/kernel/ryzen_smu_drv").as_path()) {
            if let Some(name) = MAILBOX_AND_PM_FILES
                .into_iter()
                .find(|name| path.file_name() == Some(std::ffi::OsStr::new(name)))
            {
                lock(&self.touched).push(name);
            }
        }
    }

    pub(super) fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.record(path);
        match lock(&self.entries).get(path) {
            Some(Entry::File(bytes)) => Ok(bytes.clone()),
            Some(Entry::Directory) => Err(io::Error::other("not a regular file")),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    pub(super) fn attribute(&self, path: &Path) -> io::Result<String> {
        crate::files::read_attribute_from(self.read(path).map(io::Cursor::new))
    }

    pub(super) fn list(&self, path: &Path) -> io::Result<Vec<OsString>> {
        let entries = lock(&self.entries);
        match entries.get(path) {
            Some(Entry::Directory) => Ok(entries
                .keys()
                .filter(|entry| entry.parent() == Some(path))
                .map(|entry| entry.file_name().unwrap().to_owned())
                .collect()),
            Some(Entry::File(_)) => Err(rustix::io::Errno::NOTDIR.into()),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn driver(&self, identity_only: bool) -> Driver {
        let cpu_names = self
            .list(&self.path("sys/devices/system/cpu"))
            .unwrap()
            .into_iter()
            .map(|name| name.into_string().unwrap())
            .collect();
        let loaded = lock(&self.entries).contains_key(&self.path("sys/kernel/ryzen_smu_drv"));
        let read = self.clone();
        if identity_only {
            Driver::identity_only_from_readers(self.root(), cpu_names, loaded, move |path| {
                read.read(path)
            })
            .unwrap()
        } else {
            let write = self.clone();
            Driver::from_readers(
                self.root(),
                cpu_names,
                loaded,
                move |path| read.read(path),
                move |path, bytes| {
                    write.record(path);
                    let mut entries = lock(&write.entries);
                    match entries.get_mut(path) {
                        Some(Entry::File(content)) => {
                            if content.len() < bytes.len() {
                                content.resize(bytes.len(), 0);
                            }
                            content[..bytes.len()].copy_from_slice(bytes);
                            Ok(bytes.len())
                        }
                        Some(Entry::Directory) => Err(io::Error::other("not a regular file")),
                        None => Err(io::ErrorKind::NotFound.into()),
                    }
                },
            )
            .unwrap()
        }
    }

    pub(super) fn ranking(
        &self,
        cores: &[CoreInfo],
    ) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        crate::ranking::read_with(self.root(), cores, |path| self.attribute(path))
    }

    pub(super) fn watchdog(&self) -> Check {
        crate::watchdog::probe_with(
            self.root(),
            |path| self.list(path).map(|entries| entries.into_iter().map(Ok)),
            |path| self.attribute(path),
        )
    }

    #[cfg(target_os = "linux")]
    pub(super) fn materialize(&self) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        for (path, entry) in lock(&self.entries).iter() {
            let Ok(relative) = path.strip_prefix(self.root()) else {
                continue;
            };
            let path = dir.path().join(relative);
            match entry {
                Entry::Directory => std::fs::create_dir_all(path).unwrap(),
                Entry::File(bytes) => std::fs::write(path, bytes).unwrap(),
            }
        }
        dir
    }
}

impl HostReadings for Tree {
    fn watchdog(&self, _root: &Path) -> Check {
        self.watchdog()
    }

    fn ranking(
        &self,
        _root: &Path,
        cores: &[CoreInfo],
    ) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        self.ranking(cores)
    }
}

#[cfg(target_os = "linux")]
pub(super) struct Watch {
    touched: Arc<Mutex<Vec<&'static str>>>,
    start: usize,
}

#[cfg(target_os = "linux")]
impl Watch {
    pub(super) fn on(tree: &Tree) -> Self {
        Self {
            touched: Arc::clone(&tree.touched),
            start: lock(&tree.touched).len(),
        }
    }

    pub(super) fn touched(&self) -> Vec<&'static str> {
        lock(&self.touched)[self.start..]
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[cfg(target_os = "linux")]
pub(super) fn resolved_account() -> Account {
    Account::Resolved {
        name: "trial-user".to_owned(),
        identity: BackendIdentity::new(123, 456).unwrap(),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn refused_account() -> Account {
    Account::Refused(IdentityError::NotConfigured)
}

#[cfg(target_os = "linux")]
/// A scope probe that counts its calls and answers with a canned reply.
#[derive(Clone)]
pub(super) struct Scope {
    pub(super) calls: Rc<Cell<usize>>,
    reply: Rc<dyn Fn() -> Result<String, togi_trial::Error>>,
}

#[cfg(target_os = "linux")]
impl Scope {
    pub(super) fn creating() -> Self {
        Self::answering(|| Ok("scope confined to cpu 0 created".to_owned()))
    }

    /// A probe whose `systemd-run` exited with status 1, writing `stderr`.
    pub(super) fn refusing(stderr: &'static str) -> Self {
        use std::os::unix::process::ExitStatusExt as _;

        Self::answering(move || {
            Err(togi_trial::Error::Probe(togi_trial::SystemdError::Exit {
                status: std::process::ExitStatus::from_raw(1 << 8),
                output: stderr.to_owned(),
            }))
        })
    }

    pub(super) fn failing(error: fn() -> togi_trial::Error) -> Self {
        Self::answering(move || Err(error()))
    }

    fn answering(reply: impl Fn() -> Result<String, togi_trial::Error> + 'static) -> Self {
        Self {
            calls: Rc::default(),
            reply: Rc::new(reply),
        }
    }
}

#[cfg(target_os = "linux")]
impl ScopeProbe for Scope {
    fn probe(&self) -> Result<String, togi_trial::Error> {
        self.calls.set(self.calls.get() + 1);
        (self.reply)()
    }
}

#[cfg(target_os = "linux")]
pub(super) fn host(
    tree: &Tree,
    privilege: Privilege,
    account: Account,
    scope: Scope,
) -> SystemHost<Tree> {
    host_with_driver(tree, tree.driver(false), privilege, account, scope)
}

#[cfg(target_os = "linux")]
pub(super) fn host_with_driver(
    tree: &Tree,
    driver: Driver,
    privilege: Privilege,
    account: Account,
    scope: Scope,
) -> SystemHost<Tree> {
    let mut parts = Parts::new(driver, &Config::default()).unwrap();
    parts.account = account;
    SystemHost {
        root: tree.root().to_owned(),
        privilege,
        driver: parts.driver,
        pm: parts.pm,
        backends: parts.backends,
        account: parts.account,
        scope: Box::new(scope),
        readings: tree.clone(),
    }
}
