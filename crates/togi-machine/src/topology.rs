use crate::object_only;
use crate::{CcdId, CoreId, CpuId};
use serde::Serialize;

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CoreInfo {
        pub core: CoreId,
        pub ccd: CcdId,
        pub cpus: Vec<CpuId>,
    }
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

object_only! {
    #[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Serialize)]
    pub struct BiosContext {
        pub bios_version: String,
        pub board: String,
        pub cpu_model: String,
        pub microcode: String,
        pub boost_limit_mhz: u32,
    }
}

/// A field of [`BiosContext`], named as it is recorded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextField {
    BiosVersion,
    Board,
    CpuModel,
    Microcode,
    BoostLimitMhz,
}

impl std::fmt::Display for ContextField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BiosVersion => "bios_version",
            Self::Board => "board",
            Self::CpuModel => "cpu_model",
            Self::Microcode => "microcode",
            Self::BoostLimitMhz => "boost_limit_mhz",
        })
    }
}

/// A field of the BIOS context that differs from the one a session recorded.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{field} is {current}; the session recorded {recorded}")]
pub struct ContextMismatch {
    pub field: ContextField,
    pub current: String,
    pub recorded: String,
}

impl BiosContext {
    /// Reports the first field of `current` that differs from `self`, the recorded context.
    pub fn first_difference(&self, current: &Self) -> Option<ContextMismatch> {
        let texts = [
            (
                ContextField::BiosVersion,
                &self.bios_version,
                &current.bios_version,
            ),
            (ContextField::Board, &self.board, &current.board),
            (ContextField::CpuModel, &self.cpu_model, &current.cpu_model),
            (ContextField::Microcode, &self.microcode, &current.microcode),
        ];
        for (field, recorded, now) in texts {
            if recorded != now {
                return Some(ContextMismatch {
                    field,
                    current: now.clone(),
                    recorded: recorded.clone(),
                });
            }
        }
        (self.boost_limit_mhz != current.boost_limit_mhz).then(|| ContextMismatch {
            field: ContextField::BoostLimitMhz,
            current: current.boost_limit_mhz.to_string(),
            recorded: self.boost_limit_mhz.to_string(),
        })
    }
}
