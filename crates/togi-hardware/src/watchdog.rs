use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use togi_machine::{Check, WatchdogState, error_chain};

use crate::checks::{CheckName, failed, passed};
use crate::files::read_attribute;

const CLASS_DIR: &str = "sys/class/watchdog";
const SOFTWARE: &str = "Software Watchdog";

#[derive(Debug, thiserror::Error)]
enum Refusal {
    #[error("find hardware watchdogs")]
    List(#[source] io::Error),
    #[error("read {name} state")]
    ReadState {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("{name} state {state:?}, want active")]
    NotActive { name: String, state: String },
    #[error("read {name} identity")]
    ReadIdentity {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("{name} is not a hardware watchdog ({identity:?})")]
    NotHardware { name: String, identity: String },
    #[error("read PID 1 descriptors")]
    ReadPid1(#[source] io::Error),
}

/// The watchdog device the sysfs probe validated as active hardware.
pub(crate) struct Ready {
    pub(crate) name: String,
    pub(crate) identity: String,
}

impl Ready {
    fn detail(&self) -> String {
        format!("{} ({}) active", self.name, self.identity)
    }

    /// Whether `path` is this device's node: `/dev/<name>`, or the legacy `/dev/watchdog`, which the kernel
    /// registers for `watchdog0` only.
    fn is_node(&self, path: &Path) -> bool {
        let Some(node) = path.to_str().and_then(|path| path.strip_prefix("/dev/")) else {
            return false;
        };
        node == self.name || (node == "watchdog" && self.name == "watchdog0")
    }
}

/// Every watchdog device the sysfs probe validated as active hardware, in name order; never empty.
pub(crate) struct Active {
    pub(crate) first: Ready,
    pub(crate) rest: Vec<Ready>,
}

impl Active {
    fn devices(&self) -> impl Iterator<Item = &Ready> {
        std::iter::once(&self.first).chain(&self.rest)
    }

    fn detail(&self) -> String {
        self.devices()
            .map(Ready::detail)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn devices(class: &Path) -> io::Result<impl Iterator<Item = io::Result<OsString>> + use<>> {
    Ok(std::fs::read_dir(class)?.map(|entry| entry.map(|entry| entry.file_name())))
}

fn examine(
    name: &str,
    dir: &Path,
    read_attribute: &impl Fn(&Path) -> io::Result<String>,
) -> Result<Option<Ready>, Refusal> {
    let state = match read_attribute(&dir.join("state")) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Refusal::ReadState {
                name: name.to_owned(),
                source,
            });
        }
    };
    let state = state.trim();
    if state != "active" {
        return Err(Refusal::NotActive {
            name: name.to_owned(),
            state: state.to_owned(),
        });
    }
    let identity =
        read_attribute(&dir.join("identity")).map_err(|source| Refusal::ReadIdentity {
            name: name.to_owned(),
            source,
        })?;
    let identity = identity.trim();
    if identity.is_empty() || identity == SOFTWARE {
        return Err(Refusal::NotHardware {
            name: name.to_owned(),
            identity: identity.to_owned(),
        });
    }
    Ok(Some(Ready {
        name: name.to_owned(),
        identity: identity.to_owned(),
    }))
}

/// Whether some watchdog under `root` is active and not the software one. Only observes the
/// kernel's report: an identity other than `Software Watchdog` is not proof of a physical timer.
pub(crate) fn probe(root: &Path) -> Check {
    probe_with(root, devices, read_attribute)
}

pub(crate) fn probe_with<I>(
    root: &Path,
    list: impl Fn(&Path) -> io::Result<I>,
    read_attribute: impl Fn(&Path) -> io::Result<String>,
) -> Check
where
    I: IntoIterator<Item = io::Result<OsString>>,
{
    match scan_with(root, list, read_attribute) {
        Ok(active) => passed(CheckName::Watchdog, active.first.detail()),
        Err(check) => check,
    }
}

/// Every active hardware watchdog under `root`, or the failed check saying why there is none.
pub(crate) fn scan(root: &Path) -> Result<Active, Check> {
    scan_with(root, devices, read_attribute)
}

fn scan_with<I>(
    root: &Path,
    list: impl Fn(&Path) -> io::Result<I>,
    read_attribute: impl Fn(&Path) -> io::Result<String>,
) -> Result<Active, Check>
where
    I: IntoIterator<Item = io::Result<OsString>>,
{
    let class = root.join(CLASS_DIR);
    let entries = match list(&class) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(failed(
                CheckName::Watchdog,
                error_chain(&Refusal::List(error)),
            ));
        }
    };
    let mut devices = Vec::new();
    for entry in entries.into_iter().flatten() {
        let name = match entry {
            Ok(name) => name,
            Err(error) => {
                return Err(failed(
                    CheckName::Watchdog,
                    error_chain(&Refusal::List(error)),
                ));
            }
        };
        if let Some(name) = name.to_str().filter(|name| name.starts_with("watchdog")) {
            devices.push((name.to_owned(), class.join(name)));
        }
    }
    devices.sort();
    let mut refusals = Vec::new();
    let mut ready = Vec::new();
    for (name, dir) in &devices {
        match examine(name, dir, &read_attribute) {
            Ok(Some(device)) => ready.push(device),
            Ok(None) => {}
            Err(refusal) => refusals.push(error_chain(&refusal)),
        }
    }
    let mut ready = ready.into_iter();
    if let Some(first) = ready.next() {
        return Ok(Active {
            first,
            rest: ready.collect(),
        });
    }
    if refusals.is_empty() {
        return Err(failed(
            CheckName::Watchdog,
            "no hardware watchdog state available",
        ));
    }
    Err(failed(CheckName::Watchdog, refusals.join("; ")))
}

/// The device paths PID 1's open descriptors point at. A descriptor that closes between the listing and its
/// readlink is skipped: PID 1 closes and opens descriptors while it runs.
pub(crate) fn pid1_descriptors(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut targets = Vec::new();
    for entry in std::fs::read_dir(root.join("proc/1/fd"))? {
        if let Ok(target) = std::fs::read_link(entry?.path()) {
            targets.push(target);
        }
    }
    Ok(targets)
}

/// Whether PID 1's hardware watchdog is armed under `root`: a hardware watchdog is active (`scan`) and PID 1
/// holds one of the active devices, which is what feeds it. Observes only what the kernel reports.
pub(crate) fn arming(root: &Path) -> WatchdogState {
    arming_with(&scan(root), pid1_descriptors(root))
}

pub(crate) fn arming_with(
    active: &Result<Active, Check>,
    pid1_descriptors: io::Result<Vec<PathBuf>>,
) -> WatchdogState {
    let active = match active {
        Ok(active) => active,
        Err(check) => {
            return WatchdogState::Unarmed {
                detail: check.detail.clone(),
            };
        }
    };
    let held = match pid1_descriptors {
        Ok(held) => held,
        Err(error) => {
            return WatchdogState::Unarmed {
                detail: format!(
                    "{}; {}",
                    active.detail(),
                    error_chain(&Refusal::ReadPid1(error))
                ),
            };
        }
    };
    match active
        .devices()
        .find(|device| held.iter().any(|path| device.is_node(path)))
    {
        Some(device) => WatchdogState::Armed {
            detail: format!("{}, held by PID 1", device.detail()),
        },
        None => WatchdogState::Unarmed {
            detail: format!(
                "{}, but PID 1 does not hold {}",
                active.detail(),
                active
                    .devices()
                    .map(|device| format!("/dev/{}", device.name))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        },
    }
}
