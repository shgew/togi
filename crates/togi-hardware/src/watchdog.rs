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

struct Ready {
    name: String,
    identity: String,
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
    let class = root.join(CLASS_DIR);
    let entries = match list(&class) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return failed(CheckName::Watchdog, error_chain(&Refusal::List(error))),
    };
    let mut devices = Vec::new();
    for entry in entries.into_iter().flatten() {
        let name = match entry {
            Ok(name) => name,
            Err(error) => return failed(CheckName::Watchdog, error_chain(&Refusal::List(error))),
        };
        if let Some(name) = name.to_str().filter(|name| name.starts_with("watchdog")) {
            devices.push((name.to_owned(), class.join(name)));
        }
    }
    devices.sort();
    let mut refusals = Vec::new();
    for (name, dir) in &devices {
        match examine(name, dir, &read_attribute) {
            Ok(Some(ready)) => {
                return passed(
                    CheckName::Watchdog,
                    format!("{} ({}) active", ready.name, ready.identity),
                );
            }
            Ok(None) => {}
            Err(refusal) => refusals.push(error_chain(&refusal)),
        }
    }
    if refusals.is_empty() {
        return failed(CheckName::Watchdog, "no hardware watchdog state available");
    }
    failed(CheckName::Watchdog, refusals.join("; "))
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

/// Whether PID 1's hardware watchdog is armed under `root`: some hardware watchdog is active (`probe`) and PID 1
/// holds a watchdog device, which is what feeds it. Observes only what the kernel reports.
pub(crate) fn arming(root: &Path) -> WatchdogState {
    arming_with(&probe(root), pid1_descriptors(root))
}

pub(crate) fn arming_with(
    active: &Check,
    pid1_descriptors: io::Result<Vec<PathBuf>>,
) -> WatchdogState {
    if !active.ok {
        return WatchdogState::Unarmed {
            detail: active.detail.clone(),
        };
    }
    match pid1_descriptors {
        Err(error) => WatchdogState::Unarmed {
            detail: format!(
                "{}; {}",
                active.detail,
                error_chain(&Refusal::ReadPid1(error))
            ),
        },
        Ok(held) if held.iter().any(|path| is_watchdog_device(path)) => WatchdogState::Armed {
            detail: format!("{}, held by PID 1", active.detail),
        },
        Ok(_) => WatchdogState::Unarmed {
            detail: format!("{}, but PID 1 holds no /dev/watchdog device", active.detail),
        },
    }
}

/// `/dev/watchdog` or `/dev/watchdogN`; a component-wise prefix test would miss the numbered devices.
fn is_watchdog_device(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|path| path.starts_with("/dev/watchdog"))
}
