use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use togi_machine::{BootId, CpuFrequency, CpuId, FrequencyPolicy, FrequencyPolicyId};

/// Linux cpufreq adapter. Policy membership comes from `related_cpus`, not from a
/// CPU's symlink or an assumption that one policy equals one physical core.
#[derive(Clone, Debug)]
pub struct Sysfs {
    root: PathBuf,
    boot: PathBuf,
}

impl Default for Sysfs {
    fn default() -> Self {
        Self {
            root: "/sys/devices/system/cpu/cpufreq".into(),
            boot: "/proc/sys/kernel/random/boot_id".into(),
        }
    }
}

impl Sysfs {
    fn path(&self, id: FrequencyPolicyId, name: &str) -> PathBuf {
        self.root.join(format!("policy{}", id.get())).join(name)
    }

    fn number(&self, id: FrequencyPolicyId, name: &str) -> io::Result<u32> {
        number(&std::fs::read_to_string(self.path(id, name))?)
    }
}

impl CpuFrequency for Sysfs {
    fn boot_id(&self) -> io::Result<BootId> {
        let boot = std::fs::read_to_string(&self.boot)?;
        let boot = boot.trim();
        if boot.is_empty() {
            return Err(super::invalid("kernel boot id is empty"));
        }
        Ok(BootId::from(boot.to_owned()))
    }

    fn policies(&self) -> io::Result<Vec<FrequencyPolicy>> {
        let mut policies = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|name| name.strip_prefix("policy")) else {
                continue;
            };
            let id = FrequencyPolicyId::new(name.parse().map_err(super::invalid_parse)?);
            let cpus = std::fs::read_to_string(self.path(id, "related_cpus"))?;
            let available =
                match std::fs::read_to_string(self.path(id, "scaling_available_frequencies")) {
                    Ok(values) => Some(
                        values
                            .split_whitespace()
                            .map(number)
                            .collect::<io::Result<Vec<_>>>()?,
                    ),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error),
                };
            policies.push(FrequencyPolicy {
                id,
                related_cpus: related_cpus(&cpus)?,
                cpuinfo_min_khz: self.number(id, "cpuinfo_min_freq")?,
                scaling_min_khz: self.number(id, "scaling_min_freq")?,
                scaling_max_khz: self.number(id, "scaling_max_freq")?,
                available_khz: available,
            });
        }
        policies.sort_unstable_by_key(|policy| policy.id);
        Ok(policies)
    }

    fn scaling_max_khz(&self, policy: FrequencyPolicyId) -> io::Result<u32> {
        self.number(policy, "scaling_max_freq")
    }

    fn set_scaling_max_khz(&self, policy: FrequencyPolicyId, khz: u32) -> io::Result<()> {
        if khz == 0 {
            return Err(super::invalid("zero is not a frequency limit"));
        }
        write_limit(&self.path(policy, "scaling_max_freq"), khz)
    }
}

fn write_limit(path: &Path, khz: u32) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    writeln!(file, "{khz}")
}

fn number(value: &str) -> io::Result<u32> {
    let value: u32 = value.trim().parse().map_err(super::invalid_parse)?;
    if value == 0 {
        return Err(super::invalid("zero is not a frequency limit"));
    }
    Ok(value)
}

fn related_cpus(value: &str) -> io::Result<Vec<CpuId>> {
    let mut cpus = Vec::new();
    // related_cpus normally uses spaces; accepting the kernel's list form as well avoids
    // coupling discovery to a driver's choice between cpulist and cpumask formatting.
    for part in value
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
    {
        let (first, last) = if let Some((first, last)) = part.split_once('-') {
            (
                first.parse::<u16>().map_err(super::invalid_parse)?,
                last.parse::<u16>().map_err(super::invalid_parse)?,
            )
        } else {
            let cpu = part.parse::<u16>().map_err(super::invalid_parse)?;
            (cpu, cpu)
        };
        if last < first {
            return Err(super::invalid("reversed related_cpus range"));
        }
        cpus.extend((first..=last).map(CpuId::from));
    }
    cpus.sort_unstable();
    cpus.dedup();
    if cpus.is_empty() {
        return Err(super::invalid("cpufreq policy has no related CPUs"));
    }
    Ok(cpus)
}

#[cfg(test)]
mod tests {
    use super::related_cpus;
    use togi_machine::CpuId;

    #[test]
    fn parses_related_cpu_membership() {
        assert_eq!(
            related_cpus("0 2 16 18\n").unwrap(),
            vec![
                CpuId::from(0),
                CpuId::from(2),
                CpuId::from(16),
                CpuId::from(18)
            ]
        );
        assert_eq!(
            related_cpus("0-2,16-18\n").unwrap(),
            vec![
                CpuId::from(0),
                CpuId::from(1),
                CpuId::from(2),
                CpuId::from(16),
                CpuId::from(17),
                CpuId::from(18)
            ]
        );
    }

    #[test_case::test_case("")]
    #[test_case::test_case("65536")]
    #[test_case::test_case("-1")]
    #[test_case::test_case("4-2")]
    fn refuses_invalid_cpu_membership(value: &str) {
        assert!(related_cpus(value).is_err());
    }
}
