use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use togi_machine::{CancellationToken, Check, WatchdogState, error_chain};

use crate::checks::{CheckName, failed, passed};
use crate::files::read_attribute;

const CLASS_DIR: &str = "sys/class/watchdog";
const SOFTWARE: &str = "Software Watchdog";

// One refresh and a coarse counter's equal reading on either side use three
// intervals. The fourth must decrease: PID 1 waits at least a quarter timeout
// between pings, so the shipped 30 s and 60 s timeouts allow at most one refresh
// in four seconds, and a value persisting under two seconds cannot repeat over
// two consecutive refresh-free intervals.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const SAMPLE_INTERVALS: usize = 4;

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("watchdog sampling interrupted")]
pub(crate) struct Cancelled;

#[derive(Debug, thiserror::Error)]
enum ExamineError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

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
    #[error("{name} open but not counting: read {attribute}")]
    ReadCounter {
        name: String,
        attribute: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{name} open but not counting: timeleft {timeleft} s exceeds timeout {timeout} s")]
    NotCounting {
        name: String,
        timeleft: u64,
        timeout: u64,
    },
    #[error("{name} open but not counting: timeleft did not decrease between samples")]
    NoProgress { name: String },
    #[error("read PID 1 descriptors")]
    ReadPid1(#[source] io::Error),
}

/// An active hardware watchdog with observed countdown progress.
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

/// Every hardware watchdog passing the shared probe, in name order; never empty.
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
    wait: &impl Fn(Duration) -> Result<(), Cancelled>,
) -> Result<Option<Ready>, ExamineError> {
    let state = match read_attribute(&dir.join("state")) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Refusal::ReadState {
                name: name.to_owned(),
                source,
            }
            .into());
        }
    };
    let state = state.trim();
    if state != "active" {
        return Err(Refusal::NotActive {
            name: name.to_owned(),
            state: state.to_owned(),
        }
        .into());
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
        }
        .into());
    }
    let counter = |attribute| {
        read_attribute(&dir.join(attribute))
            .and_then(|text| {
                text.trim()
                    .parse::<u64>()
                    .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))
            })
            .map_err(|source| Refusal::ReadCounter {
                name: name.to_owned(),
                attribute,
                source,
            })
    };
    let timeout = counter("timeout")?;
    let sample = || {
        let timeleft = counter("timeleft")?;
        if timeleft > timeout {
            return Err(Refusal::NotCounting {
                name: name.to_owned(),
                timeleft,
                timeout,
            });
        }
        Ok(timeleft)
    };
    let mut previous = sample()?;
    for _ in 0..SAMPLE_INTERVALS {
        wait(SAMPLE_INTERVAL)?;
        let current = sample()?;
        if current < previous {
            return Ok(Some(Ready {
                name: name.to_owned(),
                identity: identity.to_owned(),
            }));
        }
        // PID 1 may have petted the watchdog between samples, even when the
        // rounded seconds are equal. Require a later decrease, not the pet itself.
        previous = current;
    }
    Err(Refusal::NoProgress {
        name: name.to_owned(),
    }
    .into())
}

/// Whether some hardware watchdog under `root` is active with observed countdown progress.
/// Missing, unreadable, implausible or nondecreasing counters fail.
pub(crate) fn probe(root: &Path, cancel: &CancellationToken) -> Result<Check, Cancelled> {
    probe_with(root, devices, read_attribute, |duration| {
        wait(cancel, duration)
    })
}

fn wait(cancel: &CancellationToken, duration: Duration) -> Result<(), Cancelled> {
    if cancel.wait_timeout(duration) {
        Err(Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn probe_with<I>(
    root: &Path,
    list: impl Fn(&Path) -> io::Result<I>,
    read_attribute: impl Fn(&Path) -> io::Result<String>,
    wait: impl Fn(Duration) -> Result<(), Cancelled>,
) -> Result<Check, Cancelled>
where
    I: IntoIterator<Item = io::Result<OsString>>,
{
    Ok(match scan_with(root, list, read_attribute, wait)? {
        Ok(active) => passed(CheckName::Watchdog, active.first.detail()),
        Err(check) => check,
    })
}

/// Every active hardware watchdog under `root`, or the failed check saying why there is none.
pub(crate) fn scan(
    root: &Path,
    cancel: &CancellationToken,
) -> Result<Result<Active, Check>, Cancelled> {
    scan_with(root, devices, read_attribute, |duration| {
        wait(cancel, duration)
    })
}

fn scan_with<I>(
    root: &Path,
    list: impl Fn(&Path) -> io::Result<I>,
    read_attribute: impl Fn(&Path) -> io::Result<String>,
    wait: impl Fn(Duration) -> Result<(), Cancelled>,
) -> Result<Result<Active, Check>, Cancelled>
where
    I: IntoIterator<Item = io::Result<OsString>>,
{
    let class = root.join(CLASS_DIR);
    let entries = match list(&class) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Ok(Err(failed(
                CheckName::Watchdog,
                error_chain(&Refusal::List(error)),
            )));
        }
    };
    let mut devices = Vec::new();
    for entry in entries.into_iter().flatten() {
        let name = match entry {
            Ok(name) => name,
            Err(error) => {
                return Ok(Err(failed(
                    CheckName::Watchdog,
                    error_chain(&Refusal::List(error)),
                )));
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
        match examine(name, dir, &read_attribute, &wait) {
            Ok(Some(device)) => ready.push(device),
            Ok(None) => {}
            Err(ExamineError::Refused(refusal)) => refusals.push(error_chain(&refusal)),
            Err(ExamineError::Cancelled(cancelled)) => return Err(cancelled),
        }
    }
    let mut ready = ready.into_iter();
    if let Some(first) = ready.next() {
        return Ok(Ok(Active {
            first,
            rest: ready.collect(),
        }));
    }
    if refusals.is_empty() {
        return Ok(Err(failed(
            CheckName::Watchdog,
            "no hardware watchdog state available",
        )));
    }
    Ok(Err(failed(CheckName::Watchdog, refusals.join("; "))))
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

/// Whether PID 1 holds a hardware watchdog passing the shared probe under `root` (`scan`).
/// The shared probe also supplies preflight, so every armed check rejects a stopped MMIO timer.
pub(crate) fn arming(root: &Path, cancel: &CancellationToken) -> Result<WatchdogState, Cancelled> {
    Ok(arming_with(&scan(root, cancel)?, pid1_descriptors(root)))
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
