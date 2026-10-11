use crate::{CcdId, CoreId, CpuId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoreInfo {
    pub core: CoreId,
    pub ccd: CcdId,
    pub cpus: Vec<CpuId>,
}

pub fn order(cores: &[CoreInfo]) -> Vec<CoreId> {
    let mut sorted: Vec<_> = cores.iter().collect();
    sorted.sort_by_key(|core| (core.ccd, core.core));
    let mut result = Vec::with_capacity(cores.len());
    for index in 0..cores.len() {
        let before = result.len();
        for ccd in sorted.chunk_by(|a, b| a.ccd == b.ccd) {
            if let Some(core) = ccd.get(index) {
                result.push(core.core);
            }
        }
        if result.len() == before {
            break;
        }
    }
    result
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosContext {
    pub bios_version: String,
    pub board: String,
    pub cpu_model: String,
    pub microcode: String,
    pub boost_limit_mhz: i64,
}

/// A field of the BIOS context that differs from the one a session recorded.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{field} is {current}; the session recorded {recorded}")]
pub struct ContextMismatch {
    pub field: &'static str,
    pub current: String,
    pub recorded: String,
}

/// Reports the first field of `current` that differs from `recorded`.
pub fn compare_context(
    recorded: &BiosContext,
    current: &BiosContext,
) -> Result<(), ContextMismatch> {
    let texts = [
        (
            "bios_version",
            &recorded.bios_version,
            &current.bios_version,
        ),
        ("board", &recorded.board, &current.board),
        ("cpu_model", &recorded.cpu_model, &current.cpu_model),
        ("microcode", &recorded.microcode, &current.microcode),
    ];
    for (field, before, after) in texts {
        if before != after {
            return Err(ContextMismatch {
                field,
                current: after.clone(),
                recorded: before.clone(),
            });
        }
    }
    if recorded.boost_limit_mhz != current.boost_limit_mhz {
        return Err(ContextMismatch {
            field: "boost_limit_mhz",
            current: current.boost_limit_mhz.to_string(),
            recorded: recorded.boost_limit_mhz.to_string(),
        });
    }
    Ok(())
}
