use std::io;
use std::num::ParseIntError;
use std::path::Path;

use togi_machine::{CoreId, CoreInfo, CoreRank, CpuId, Partial};

use crate::files::read_attribute;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RankingError {
    #[error("core {core} lists no logical CPU")]
    NoCpu { core: CoreId },
    #[error("read the preferred-core ranking of cpu {}", cpu.get())]
    Read {
        cpu: CpuId,
        #[source]
        source: io::Error,
    },
    #[error("the preferred-core ranking of cpu {} is {text:?}, not an integer", cpu.get())]
    Parse {
        cpu: CpuId,
        text: String,
        #[source]
        source: ParseIntError,
    },
}

fn rank(
    root: &Path,
    core: &CoreInfo,
    read_attribute: &impl Fn(&Path) -> io::Result<String>,
) -> Result<CoreRank, RankingError> {
    let cpu = *core
        .cpus
        .first()
        .ok_or(RankingError::NoCpu { core: core.core })?;
    let path = root
        .join("sys/devices/system/cpu/cpufreq")
        .join(format!("policy{}", cpu.get()))
        .join("amd_pstate_prefcore_ranking");
    let text = read_attribute(&path).map_err(|source| RankingError::Read { cpu, source })?;
    let value = text.trim().parse().map_err(|source| RankingError::Parse {
        cpu,
        text: text.trim().to_owned(),
        source,
    })?;
    Ok(CoreRank {
        core: core.core,
        value,
    })
}

/// Each core's preferred-core ranking from the policy of its first logical CPU, in topology order.
/// The first failure ends the read and keeps the ranks read before it.
pub(crate) fn read(
    root: &Path,
    cores: &[CoreInfo],
) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
    read_with(root, cores, read_attribute)
}

pub(crate) fn read_with(
    root: &Path,
    cores: &[CoreInfo],
    read_attribute: impl Fn(&Path) -> io::Result<String>,
) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
    let mut ranks = Vec::with_capacity(cores.len());
    for core in cores {
        match rank(root, core, &read_attribute) {
            Ok(rank) => ranks.push(rank),
            Err(error) => {
                return Err(Partial {
                    value: ranks,
                    error: togi_machine::Error::Other(Box::new(error)),
                });
            }
        }
    }
    Ok(ranks)
}
