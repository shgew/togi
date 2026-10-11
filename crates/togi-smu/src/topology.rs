use crate::Error;
use crate::files::{is_missing, read_int, read_int_opt, read_lossy};
use crate::slots::SlotError;
use std::fs;
use std::path::Path;
use togi_machine::{CcdId, CpuId};

const CPU_DIR: &str = "sys/devices/system/cpu";

/// What the kernel reports for one online logical CPU, unvalidated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RawCpu {
    pub(crate) cpu: CpuId,
    pub(crate) core_id: i64,
    pub(crate) ccd_key: i64,
    pub(crate) package: i64,
}

/// A physical core: its raw kernel id, dense CCD index and logical CPUs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CoreGroup {
    pub(crate) core_id: i64,
    pub(crate) ccd: CcdId,
    pub(crate) cpus: Vec<CpuId>,
}

fn cpu_numbers(base: &Path) -> Result<Vec<CpuId>, Error> {
    let list = |source| Error::ListCpus {
        path: base.to_owned(),
        source,
    };
    let mut cpus = Vec::new();
    for entry in fs::read_dir(base).map_err(list)? {
        let entry = entry.map_err(list)?;
        if !entry.file_type().map_err(list)?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(digits) = name
            .to_str()
            .and_then(|name| name.strip_prefix("cpu"))
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let cpu = digits.parse().map_err(|_| Error::CpuNumber {
            name: name.to_string_lossy().into_owned(),
        })?;
        cpus.push(CpuId(cpu));
    }
    cpus.sort_unstable();
    Ok(cpus)
}

fn online(dir: &Path) -> Result<bool, Error> {
    match read_lossy(&dir.join("online")) {
        Ok(text) => Ok(text.trim() != "0"),
        Err(error) if is_missing(&error) => Ok(true),
        Err(error) => Err(error),
    }
}

pub(crate) fn read(root: &Path) -> Result<Vec<RawCpu>, Error> {
    let base = root.join(CPU_DIR);
    let mut seen = Vec::new();
    for cpu in cpu_numbers(&base)? {
        let dir = base.join(format!("cpu{}", cpu.0));
        if !online(&dir)? {
            continue;
        }
        let core_id = read_int(&dir.join("topology/core_id"))?;
        let package = read_int(&dir.join("topology/physical_package_id"))?;
        let cache_id = read_int_opt(&dir.join("cache/index3/id"))?;
        seen.push((cpu, core_id, package, cache_id, dir));
    }
    if seen.is_empty() {
        return Err(Error::NoOnlineCpus { path: base });
    }
    let use_cache_ids = seen.iter().all(|(.., cache_id, _)| cache_id.is_some());
    seen.into_iter()
        .map(|(cpu, core_id, package, cache_id, dir)| {
            let ccd_key = match cache_id {
                Some(id) if use_cache_ids => id,
                _ => read_int(&dir.join("topology/die_id"))?,
            };
            Ok(RawCpu {
                cpu,
                core_id,
                ccd_key,
                package,
            })
        })
        .collect()
}

fn group_by<K: Ord + Copy>(cpus: &[RawCpu], key: impl Fn(&RawCpu) -> K) -> Vec<(K, Vec<RawCpu>)> {
    let mut sorted = cpus.to_vec();
    sorted.sort_by_key(|cpu| (key(cpu), cpu.cpu));
    sorted
        .chunk_by(|a, b| key(a) == key(b))
        .map(|group| (key(&group[0]), group.to_vec()))
        .collect()
}

/// Groups logical CPUs into cores, refusing a machine whose identity is ambiguous: more than one
/// physical package, or siblings of one core that disagree on their CCD.
pub(crate) fn group(cpus: &[RawCpu]) -> Result<Vec<CoreGroup>, SlotError> {
    let packages = group_by(cpus, |cpu| cpu.package);
    if packages.len() > 1 {
        return Err(SlotError::Packages(
            packages
                .into_iter()
                .map(|(package, members)| (package, members.iter().map(|cpu| cpu.cpu).collect()))
                .collect(),
        ));
    }
    let mut keys: Vec<i64> = cpus.iter().map(|cpu| cpu.ccd_key).collect();
    keys.sort_unstable();
    keys.dedup();
    let cores = group_by(cpus, |cpu| cpu.core_id);
    let disagreeing: Vec<(i64, Vec<(CpuId, i64)>)> = cores
        .iter()
        .filter(|(_, members)| members.iter().any(|m| m.ccd_key != members[0].ccd_key))
        .map(|(core_id, members)| {
            (
                *core_id,
                members.iter().map(|m| (m.cpu, m.ccd_key)).collect(),
            )
        })
        .collect();
    if !disagreeing.is_empty() {
        return Err(SlotError::Siblings(disagreeing));
    }
    cores
        .into_iter()
        .map(|(core_id, members)| {
            let index = keys.partition_point(|candidate| *candidate < members[0].ccd_key);
            let ccd = u8::try_from(index)
                .map(CcdId)
                .map_err(|_| SlotError::TooManyCcds { count: keys.len() })?;
            Ok(CoreGroup {
                core_id,
                ccd,
                cpus: members.iter().map(|member| member.cpu).collect(),
            })
        })
        .collect()
}
