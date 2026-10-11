use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rustix::fd::OwnedFd;
use rustix::fs::inotify;
use rustix::io::Errno;
use tempfile::TempDir;
use togi_config::Config;
use togi_smu::Driver;
use togi_trial::{BackendIdentity, IdentityError};

use crate::assemble::Parts;
use crate::host::{Privilege, ScopeProbe, SystemHost};
use crate::preflight::Account;

pub(super) const MAILBOX_AND_PM_FILES: [&str; 5] = [
    "smu_args",
    "rsmu_cmd",
    "smn",
    "pm_table_version",
    "pm_table",
];

pub(super) struct Tree {
    dir: TempDir,
}

impl Tree {
    /// A machine whose CPU and `ryzen_smu` identity checks pass, with its mailbox and PM table
    /// files in place so that touching one can be seen.
    pub(super) fn new() -> Self {
        let tree = Self {
            dir: TempDir::new().unwrap(),
        };
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
}

/// An inotify watch on the mailbox and PM table files of a tree: any open, read or write of
/// one is an event, so reaching the hardware cannot go unnoticed.
pub(super) struct Watch {
    inotify: OwnedFd,
    files: Vec<(i32, &'static str)>,
}

impl Watch {
    pub(super) fn on(tree: &Tree) -> Self {
        let inotify =
            inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC).unwrap();
        let files = MAILBOX_AND_PM_FILES
            .into_iter()
            .map(|name| {
                let path = tree.path(&format!("sys/kernel/ryzen_smu_drv/{name}"));
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
                    let file = self.files.iter().find(|(watch, _)| *watch == event.wd());
                    touched.insert(file.map_or("an unknown watch", |(_, name)| *name));
                }
            }
        }
        touched.into_iter().collect()
    }
}

pub(super) fn resolved_account() -> Account {
    Account::Resolved {
        name: "trial-user".to_owned(),
        identity: BackendIdentity::new(123, 456).unwrap(),
    }
}

pub(super) fn refused_account() -> Account {
    Account::Refused(IdentityError::NotConfigured)
}

/// A scope probe that counts its calls and answers with a canned reply.
#[derive(Clone)]
pub(super) struct Scope {
    pub(super) calls: Rc<Cell<usize>>,
    reply: Rc<dyn Fn() -> Result<String, togi_trial::Error>>,
}

impl Scope {
    pub(super) fn creating() -> Self {
        Self::answering(|| Ok("scope confined to cpu 0 created".to_owned()))
    }

    pub(super) fn refusing(detail: &'static str) -> Self {
        Self::answering(move || {
            Err(togi_trial::Error::Probe {
                detail: detail.to_owned(),
                source: None,
            })
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

impl ScopeProbe for Scope {
    fn probe(&self) -> Result<String, togi_trial::Error> {
        self.calls.set(self.calls.get() + 1);
        (self.reply)()
    }
}

pub(super) fn host(
    tree: &Tree,
    privilege: Privilege,
    account: Account,
    scope: Scope,
) -> SystemHost {
    let driver = Driver::open(tree.root()).unwrap();
    let mut parts = Parts::new(driver, &Config::default()).unwrap();
    parts.account = account;
    parts.host(tree.root(), privilege, scope)
}
