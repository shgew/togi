//! A recoverable transaction over the controller CPUs' frequency policies.

use std::fmt;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use togi_journal::fs::Fs;
use togi_machine::{
    BootId, CancellationToken, CpuFrequency, CpuId, FrequencyPolicy, FrequencyPolicyId, OnCancel,
    object_only,
};
use tokio::sync::Notify;
use tokio::time::{Instant, sleep_until};

use crate::sweep::until_cancelled;

mod sysfs;
pub use sysfs::Sysfs;

const PENDING: &str = "controller-frequency.pending.json";
const STAGING: &str = "controller-frequency.pending.tmp";

/// How long one cap or one restore waits, in total, for its written limits to read back: the
/// cpufreq core applies a written limit from a work item that normally runs within milliseconds,
/// so half a second leaves ample room for a work queue delayed by load while the whole
/// transaction, whose policies share this one deadline, stays inside the slack the start, stop
/// and sweep bounds keep (`Timing::margin`).
const READBACK_DEADLINE: Duration = Duration::from_millis(500);

/// How long a limit that does not read back yet waits before it is read again: ten milliseconds
/// notices the applied limit soon after the work item runs while reading a policy at most fifty
/// times per deadline, a negligible load on sysfs.
const READBACK_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, thiserror::Error)]
#[error("controller frequency limits: {operation}")]
pub struct Error {
    operation: &'static str,
    #[source]
    source: io::Error,
}

impl Error {
    fn new(operation: &'static str, source: io::Error) -> Self {
        Self { operation, source }
    }
}

/// Recovery attempts every saved policy; the intent stays published until each one reads back.
#[derive(Debug, thiserror::Error)]
#[error("{failed} of {total} saved policies were not confirmed restored, first policy{policy}")]
struct Unrestored {
    failed: usize,
    total: usize,
    policy: u32,
    #[source]
    first: io::Error,
}

object_only! {
    #[derive(Serialize)]
    #[serde(deny_unknown_fields)]
    struct Pending {
        boot: BootId,
        policies: Vec<Saved>,
    }
}

object_only! {
    #[derive(Serialize)]
    #[serde(deny_unknown_fields)]
    struct Saved {
        policy: FrequencyPolicyId,
        related_cpus: Vec<CpuId>,
        original_khz: u32,
        cap_khz: u32,
        state: State,
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Intended,
    Capped,
    Restored,
}

/// The pending transaction is independent of the runner process and the trial directory.
/// Memory fixtures use the same publication protocol without making durability syscalls.
pub(crate) struct Limits {
    machine: Arc<dyn CpuFrequency>,
    storage: Storage,
    dir: PathBuf,
}

enum Storage {
    Disk,
    Memory(Arc<dyn Fs>),
}

impl Limits {
    pub(crate) fn new(
        machine: Arc<dyn CpuFrequency>,
        dir: PathBuf,
        memory: Option<Arc<dyn Fs>>,
    ) -> Self {
        Self {
            machine,
            storage: memory.map_or(Storage::Disk, Storage::Memory),
            dir,
        }
    }

    /// Every worker CPU must be disjoint from every affected policy. The caller removes only
    /// explicitly excluded host-core CPUs from `workers`, never an entire CCD or policy.
    pub(crate) async fn cap(
        &self,
        controller: &[CpuId],
        workers: &[CpuId],
        cancel: &CancellationToken,
        report: &mut Vec<String>,
    ) -> Result<(), Error> {
        self.apply(controller, workers, cancel, report)
            .await
            .map_err(|source| Error::new("save and apply the cap", source))
    }

    async fn apply(
        &self,
        controller: &[CpuId],
        workers: &[CpuId],
        cancel: &CancellationToken,
        report: &mut Vec<String>,
    ) -> io::Result<()> {
        if self.load()?.is_some() {
            return Err(invalid(
                "an earlier frequency transaction still needs restoration",
            ));
        }
        if controller.is_empty() {
            return Err(invalid("no controller CPUs were selected"));
        }
        let policies = self.machine.policies()?;
        let mut saved = Vec::new();
        for policy in &policies {
            if !policy
                .related_cpus
                .iter()
                .any(|cpu| controller.contains(cpu))
            {
                continue;
            }
            if policy.related_cpus.iter().any(|cpu| workers.contains(cpu)) {
                return Err(invalid(format!(
                    "policy{} shares tested worker CPUs {:?}",
                    policy.id.get(),
                    policy.related_cpus
                )));
            }
            saved.push(Saved {
                policy: policy.id,
                related_cpus: policy.related_cpus.clone(),
                original_khz: policy.scaling_max_khz,
                cap_khz: cap_for(policy)?,
                state: State::Intended,
            });
        }
        for cpu in controller {
            if saved
                .iter()
                .filter(|policy| policy.related_cpus.contains(cpu))
                .count()
                != 1
            {
                return Err(invalid(format!("CPU {cpu} has no unique cpufreq policy")));
            }
        }
        let mut pending = Pending {
            boot: self.machine.boot_id()?,
            policies: saved,
        };
        // Publish all originals durably before the first hardware write. Even a failing write
        // may have taken effect, so every intended policy remains recoverable until read back.
        self.save(&pending)?;
        let wait = ReadbackWait::begin(cancel);
        for index in 0..pending.policies.len() {
            let saved = &pending.policies[index];
            report.push(format!(
                "controller frequency intent: policy{} CPUs {:?}, saved scaling_max_freq {} kHz, cap {} kHz",
                saved.policy.get(), saved.related_cpus, saved.original_khz, saved.cap_khz
            ));
            self.machine
                .set_scaling_max_khz(saved.policy, saved.cap_khz)?;
            let readback = self.read_back(&wait, saved.policy, saved.cap_khz).await?;
            report.push(format!(
                "controller frequency cap readback: policy{} {} kHz (requested {} kHz)",
                saved.policy.get(),
                readback.last(saved.cap_khz),
                saved.cap_khz
            ));
            if let Readback::Unconfirmed { last, ended } = readback {
                return Err(invalid(format!(
                    "policy{} cap readback {last} kHz differs from {} kHz {ended}",
                    saved.policy.get(),
                    saved.cap_khz
                )));
            }
            pending.policies[index].state = State::Capped;
            self.save(&pending)?;
        }
        Ok(())
    }

    /// Call only after teardown or sweep proved no workload remains. A previous boot's policy
    /// values are obsolete, and are discarded without touching the current boot's policies.
    /// Cancellation ends a readback wait early, leaving that policy unconfirmed; every saved
    /// policy is still written and read.
    pub(crate) async fn restore(
        &self,
        cancel: &CancellationToken,
        report: &mut Vec<String>,
    ) -> Result<(), Error> {
        self.recover(cancel, report)
            .await
            .map_err(|source| Error::new("restore saved limits", source))
    }

    async fn recover(&self, cancel: &CancellationToken, report: &mut Vec<String>) -> io::Result<()> {
        let Some(mut pending) = self.load()? else {
            return Ok(());
        };
        if pending.boot != self.machine.boot_id()? {
            report.push("discarded obsolete controller frequency recovery intent from a previous boot; no frequency values restored".to_owned());
            return self.remove();
        }
        let policies = self.machine.policies()?;
        let total = pending.policies.len();
        let mut unrestored: Option<Unrestored> = None;
        let wait = ReadbackWait::begin(cancel);
        for index in 0..total {
            let policy = pending.policies[index].policy;
            // A policy restored in hardware whose Restored state fails to persist is restored
            // again by the next recovery; the later policies are attempted regardless.
            let result = match self
                .restore_saved(&pending.policies[index], &policies, &wait, report)
                .await
            {
                Ok(()) => {
                    pending.policies[index].state = State::Restored;
                    self.save(&pending)
                }
                Err(error) => Err(error),
            };
            let Err(error) = result else {
                continue;
            };
            report.push(format!(
                "controller frequency restore failed: policy{}: {error}",
                policy.get()
            ));
            match &mut unrestored {
                Some(unrestored) => unrestored.failed += 1,
                None => {
                    unrestored = Some(Unrestored {
                        failed: 1,
                        total,
                        policy: policy.get(),
                        first: error,
                    });
                }
            }
        }
        if let Some(unrestored) = unrestored {
            return Err(io::Error::other(unrestored));
        }
        self.remove()?;
        report.push(
            "controller frequency limits restored and read back; recovery intent cleared"
                .to_owned(),
        );
        Ok(())
    }

    /// Writes nothing to a policy that is missing or whose CPUs changed since the intent.
    async fn restore_saved(
        &self,
        saved: &Saved,
        policies: &[FrequencyPolicy],
        wait: &ReadbackWait<'_>,
        report: &mut Vec<String>,
    ) -> io::Result<()> {
        let phase = match saved.state {
            State::Intended => "write or cap readback unconfirmed",
            State::Capped => "cap read back",
            State::Restored => "restoration read back",
        };
        report.push(format!(
            "controller frequency recovery: policy{} saved {} kHz, intended cap {} kHz; {phase}",
            saved.policy.get(),
            saved.original_khz,
            saved.cap_khz,
        ));
        let Some(current) = policies.iter().find(|policy| policy.id == saved.policy) else {
            return Err(invalid("saved policy is missing"));
        };
        let mut current_cpus = current.related_cpus.clone();
        let mut saved_cpus = saved.related_cpus.clone();
        current_cpus.sort_unstable();
        saved_cpus.sort_unstable();
        if current_cpus != saved_cpus {
            return Err(invalid(format!(
                "saved policy related CPUs {saved_cpus:?} are now {current_cpus:?}"
            )));
        }
        if saved.original_khz == 0 || saved.cap_khz == 0 {
            return Err(invalid("saved policy limits are invalid"));
        }
        report.push(format!(
            "controller frequency restore intent: policy{} saved scaling_max_freq {} kHz",
            saved.policy.get(),
            saved.original_khz
        ));
        self.machine
            .set_scaling_max_khz(saved.policy, saved.original_khz)?;
        let readback = self
            .read_back(wait, saved.policy, saved.original_khz)
            .await?;
        report.push(format!(
            "controller frequency restore readback: policy{} {} kHz (saved {} kHz)",
            saved.policy.get(),
            readback.last(saved.original_khz),
            saved.original_khz
        ));
        if let Readback::Unconfirmed { ended, .. } = readback {
            return Err(invalid(format!(
                "restored frequency readback differs from the saved limit {ended}"
            )));
        }
        Ok(())
    }

    /// Reads `policy` until it shows `written`, the transaction's deadline passes or
    /// cancellation ends the wait. The cpufreq core applies a written limit asynchronously, so
    /// the first read can still show the previous one.
    async fn read_back(
        &self,
        wait: &ReadbackWait<'_>,
        policy: FrequencyPolicyId,
        written: u32,
    ) -> io::Result<Readback> {
        loop {
            let last = self.machine.scaling_max_khz(policy)?;
            if last == written {
                return Ok(Readback::Matched);
            }
            let now = Instant::now();
            if now >= wait.deadline {
                return Ok(Readback::Unconfirmed {
                    last,
                    ended: Ended::Deadline,
                });
            }
            tokio::select! {
                biased;
                () = until_cancelled(wait.cancel, &wait.woken) => {
                    return Ok(Readback::Unconfirmed { last, ended: Ended::Cancelled });
                }
                () = sleep_until((now + READBACK_POLL).min(wait.deadline)) => {}
            }
        }
    }

    fn load(&self) -> io::Result<Option<Pending>> {
        let path = self.dir.join(PENDING);
        let read = match &self.storage {
            Storage::Disk => std::fs::read(path),
            Storage::Memory(fs) => fs.read(&path),
        };
        match read {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The trials root is searchable by the backend account, so the intent is created exclusively
    /// with mode 0600 (never wider, whatever the umask) and never through a symlink, after
    /// removing whatever entry an interrupted save or anyone else left at the staging name.
    #[expect(
        clippy::disallowed_methods,
        reason = "frequency recovery intent must reach stable storage before sysfs writes"
    )]
    fn save(&self, pending: &Pending) -> io::Result<()> {
        let bytes = serde_json::to_vec(pending).map_err(io::Error::other)?;
        let staging = self.dir.join(STAGING);
        let path = self.dir.join(PENDING);
        match &self.storage {
            Storage::Disk => {
                absent(std::fs::remove_file(&staging))?;
                let mut file = std::fs::File::from(rustix::fs::open(
                    &staging,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )?);
                file.write_all(&bytes)?;
                file.sync_all()?;
                std::fs::rename(staging, path)?;
                std::fs::File::open(&self.dir)?.sync_all()
            }
            Storage::Memory(fs) => {
                absent(fs.remove_file(&staging))?;
                let mut file = fs.create_new(&staging)?;
                file.write_all(&bytes)?;
                file.sync()?;
                fs.rename(&staging, &path)?;
                fs.sync_dir(&self.dir)
            }
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "confirmed restoration durably clears the recovery intent"
    )]
    fn remove(&self) -> io::Result<()> {
        let path = self.dir.join(PENDING);
        match &self.storage {
            Storage::Disk => {
                std::fs::remove_file(path)?;
                std::fs::File::open(&self.dir)?.sync_all()
            }
            Storage::Memory(fs) => {
                fs.remove_file(&path)?;
                fs.sync_dir(&self.dir)
            }
        }
    }
}

/// The one readback deadline of a cap or a restore, and the cancellation that can end its waits
/// sooner.
struct ReadbackWait<'a> {
    deadline: Instant,
    cancel: &'a CancellationToken,
    woken: Arc<Notify>,
    _registration: OnCancel,
}

impl<'a> ReadbackWait<'a> {
    fn begin(cancel: &'a CancellationToken) -> Self {
        let woken = Arc::new(Notify::new());
        let registration = {
            let woken = Arc::clone(&woken);
            cancel.on_cancel(move || woken.notify_one())
        };
        Self {
            deadline: Instant::now() + READBACK_DEADLINE,
            cancel,
            woken,
            _registration: registration,
        }
    }
}

enum Readback {
    Matched,
    /// The limit never read back as written; `last` is what the final read showed.
    Unconfirmed { last: u32, ended: Ended },
}

impl Readback {
    /// What the final read showed, given the limit that was written.
    fn last(&self, written: u32) -> u32 {
        match self {
            Self::Matched => written,
            Self::Unconfirmed { last, .. } => *last,
        }
    }
}

/// What ended the wait for a limit that never read back as written.
#[derive(Clone, Copy)]
enum Ended {
    Deadline,
    Cancelled,
}

impl fmt::Display for Ended {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline => write!(
                formatter,
                "when the {READBACK_DEADLINE:?} readback deadline passed"
            ),
            Self::Cancelled => formatter.write_str("when cancellation ended the wait"),
        }
    }
}

fn cap_for(policy: &FrequencyPolicy) -> io::Result<u32> {
    let floor = policy.cpuinfo_min_khz.max(policy.scaling_min_khz);
    if floor == 0 || floor > policy.scaling_max_khz {
        return Err(invalid(
            "policy minimum and maximum frequency limits are inconsistent",
        ));
    }
    match &policy.available_khz {
        Some(values) => values
            .iter()
            .copied()
            .filter(|&value| value >= floor && value <= policy.scaling_max_khz)
            .min()
            .ok_or_else(|| invalid("no supported frequency respects the policy minimum")),
        None => Ok(floor),
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
pub(crate) mod fake;

#[cfg(test)]
mod tests;

fn invalid_parse(error: std::num::ParseIntError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// A staging entry that was never there is as good as one removed.
fn absent(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
