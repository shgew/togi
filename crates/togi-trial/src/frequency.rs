//! A recoverable transaction over the controller CPUs' frequency policies.

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use togi_journal::fs::Fs;
use togi_machine::{BootId, CpuFrequency, CpuId, FrequencyPolicy, FrequencyPolicyId};

mod sysfs;
pub use sysfs::Sysfs;

const PENDING: &str = "controller-frequency.pending.json";
const STAGING: &str = "controller-frequency.pending.tmp";

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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    boot: BootId,
    policies: Vec<Saved>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    policy: FrequencyPolicyId,
    related_cpus: Vec<CpuId>,
    original_khz: u32,
    cap_khz: u32,
    state: State,
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
    pub(crate) fn cap(
        &self,
        controller: &[CpuId],
        workers: &[CpuId],
        report: &mut Vec<String>,
    ) -> Result<(), Error> {
        self.apply(controller, workers, report)
            .map_err(|source| Error::new("save and apply the cap", source))
    }

    fn apply(
        &self,
        controller: &[CpuId],
        workers: &[CpuId],
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
        for index in 0..pending.policies.len() {
            let saved = &pending.policies[index];
            report.push(format!(
                "controller frequency intent: policy{} CPUs {:?}, saved scaling_max_freq {} kHz, cap {} kHz",
                saved.policy.get(), saved.related_cpus, saved.original_khz, saved.cap_khz
            ));
            self.machine
                .set_scaling_max_khz(saved.policy, saved.cap_khz)?;
            let observed = self.machine.scaling_max_khz(saved.policy)?;
            report.push(format!(
                "controller frequency cap readback: policy{} {} kHz (requested {} kHz)",
                saved.policy.get(),
                observed,
                saved.cap_khz
            ));
            if observed != saved.cap_khz {
                return Err(invalid(format!(
                    "policy{} cap readback {observed} kHz differs from {} kHz",
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
    pub(crate) fn restore(&self, report: &mut Vec<String>) -> Result<(), Error> {
        self.recover(report)
            .map_err(|source| Error::new("restore saved limits", source))
    }

    fn recover(&self, report: &mut Vec<String>) -> io::Result<()> {
        let Some(mut pending) = self.load()? else {
            return Ok(());
        };
        if pending.boot != self.machine.boot_id()? {
            report.push("discarded obsolete controller frequency recovery intent from a previous boot; no frequency values restored".to_owned());
            return self.remove();
        }
        let policies = self.machine.policies()?;
        let mut failures = Vec::new();
        for index in 0..pending.policies.len() {
            let saved = &pending.policies[index];
            let phase = match saved.state {
                State::Intended => "write or cap readback unconfirmed",
                State::Capped => "cap read back",
                State::Restored => "restoration read back",
            };
            report.push(format!(
                "controller frequency recovery: policy{} saved {} kHz, intended cap {} kHz; {phase}",
                saved.policy.get(), saved.original_khz, saved.cap_khz,
            ));
            let result = (|| {
                let Some(current) = policies.iter().find(|policy| policy.id == saved.policy) else {
                    return Err(invalid("saved policy is missing"));
                };
                let mut current_cpus = current.related_cpus.clone();
                let mut saved_cpus = saved.related_cpus.clone();
                current_cpus.sort_unstable();
                saved_cpus.sort_unstable();
                if current_cpus != saved_cpus || saved.original_khz == 0 || saved.cap_khz == 0 {
                    return Err(invalid("saved policy identity or limits are invalid"));
                }
                report.push(format!(
                    "controller frequency restore intent: policy{} saved scaling_max_freq {} kHz",
                    saved.policy.get(),
                    saved.original_khz
                ));
                self.machine
                    .set_scaling_max_khz(saved.policy, saved.original_khz)?;
                let observed = self.machine.scaling_max_khz(saved.policy)?;
                report.push(format!(
                    "controller frequency restore readback: policy{} {} kHz (saved {} kHz)",
                    saved.policy.get(),
                    observed,
                    saved.original_khz
                ));
                if observed != saved.original_khz {
                    return Err(invalid(
                        "restored frequency readback differs from the saved limit",
                    ));
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    pending.policies[index].state = State::Restored;
                    self.save(&pending)?;
                }
                Err(error) => failures.push(format!("policy{}: {error}", saved.policy.get())),
            }
        }
        if !failures.is_empty() {
            return Err(io::Error::other(failures.join("; ")));
        }
        self.remove()?;
        report.push(
            "controller frequency limits restored and read back; recovery intent cleared"
                .to_owned(),
        );
        Ok(())
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
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&staging)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                std::fs::rename(staging, path)?;
                std::fs::File::open(&self.dir)?.sync_all()
            }
            Storage::Memory(fs) => {
                let mut file = fs.create_truncate(&staging)?;
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
