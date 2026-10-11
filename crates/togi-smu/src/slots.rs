use crate::mailbox::MailboxError;
use crate::topology::{self, RawCpu};
use std::fmt::{self, Write as _};
use togi_machine::{CcdId, CoreId, CpuId, PER_CORE_MAX};

const RSMU_RESPONSE: u32 = 0x03b1_0570;
const RSMU_COMMAND: u32 = 0x03b1_0524;
const CCD_FUSE: u32 = 0x304a_03dc;
const CCD_FUSE_STRIDE_SHIFT: u32 = 25;
const SLOTS_PER_CCD: usize = 8;

/// Where the SMU addresses a core: the CCD and the slot on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Location {
    pub(crate) ccd: CcdId,
    pub(crate) slot: u8,
}

impl Location {
    pub(crate) fn argument(self) -> u32 {
        u32::from(self.ccd.0) << 28 | u32::from(self.slot) << 20
    }
}

#[derive(Debug)]
pub(crate) struct SlotMap {
    locations: [Option<Location>; PER_CORE_MAX],
    detail: String,
}

impl SlotMap {
    pub(crate) fn get(&self, core: CoreId) -> Option<Location> {
        self.locations[core.index()]
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }
}

/// The four reads that verify one CCD's fuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FuseRead {
    ResponseProbe,
    Fuse,
    CommandProbe,
    RepeatFuse,
}

impl fmt::Display for FuseRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ResponseProbe => "read RSMU response before fuse",
            Self::Fuse => "read fuse",
            Self::CommandProbe => "read RSMU command before fuse",
            Self::RepeatFuse => "repeat fuse read",
        })
    }
}

/// A core whose raw id does not match the slot its position on the CCD implies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Misplaced {
    pub core: CoreId,
    pub slot: usize,
    pub want: usize,
}

fn join<T>(items: &[T], each: impl Fn(&T) -> String, separator: &str) -> String {
    items.iter().map(each).collect::<Vec<_>>().join(separator)
}

fn cpu_list(cpus: &[CpuId]) -> String {
    join(cpus, |cpu| format!("cpu{}", cpu.0), ", ")
}

#[derive(Debug, thiserror::Error)]
pub enum SlotError {
    #[error(
        "CPUs span {} physical packages: {}",
        .0.len(),
        join(.0, |(package, cpus)| format!("package {package}: {}", cpu_list(cpus)), "; ")
    )]
    Packages(Vec<(i64, Vec<CpuId>)>),
    #[error(
        "core {core_id} siblings report different CCD keys: {}",
        join(cpus, |(cpu, key)| format!("cpu{} key {key}", cpu.0), ", ")
    )]
    Siblings {
        core_id: i64,
        cpus: Vec<(CpuId, i64)>,
    },
    #[error("{count} distinct CCD keys do not fit a CCD index")]
    TooManyCcds { count: usize },
    #[error(
        "{} outside supported range 0-{}",
        join(.0, |(ccd, ids)| format!("CCD{} core IDs {}", ccd.0, join(ids, ToString::to_string, ", ")), "; "),
        PER_CORE_MAX - 1
    )]
    OutOfRange(Vec<(CcdId, Vec<i64>)>),
    #[error("{read} of CCD{}", ccd.0)]
    Read {
        ccd: CcdId,
        read: FuseRead,
        #[source]
        source: MailboxError,
    },
    #[error("CCD{} fuse reads cannot be verified: RSMU probes both returned {probe:#x}", ccd.0)]
    UnverifiableFuse { ccd: CcdId, probe: u32 },
    #[error("CCD{} fuse reads disagree: {first:#x} and {second:#x}", ccd.0)]
    FuseDisagree { ccd: CcdId, first: u32, second: u32 },
    #[error("CCD{} fuse {mask:#04x}: harvested CCDs are not yet supported", ccd.0)]
    Harvested { ccd: CcdId, mask: u8 },
    #[error("CCD{} fuse {mask:#04x} leaves 8 live slots for {cores} cores", ccd.0)]
    CoreCount { ccd: CcdId, mask: u8, cores: usize },
    #[error(
        "CCD{} core IDs disagree with modulo-eight slots: {}",
        ccd.0,
        join(misplaced, |m| format!("core {:02} at slot {}, want slot {}", m.core, m.slot, m.want), "; ")
    )]
    SlotOrder {
        ccd: CcdId,
        misplaced: Vec<Misplaced>,
    },
}

type SmnRead<'a> = dyn FnMut(u32) -> Result<u32, MailboxError> + 'a;

fn read_disabled_mask(ccd: CcdId, smn: &mut SmnRead<'_>) -> Result<u8, SlotError> {
    let mut read = |address, which| {
        smn(address).map_err(|source| SlotError::Read {
            ccd,
            read: which,
            source,
        })
    };
    let address = CCD_FUSE.wrapping_add(u32::from(ccd.0) << CCD_FUSE_STRIDE_SHIFT);
    let first_probe = read(RSMU_RESPONSE, FuseRead::ResponseProbe)?;
    let fuse = read(address, FuseRead::Fuse)?;
    let second_probe = read(RSMU_COMMAND, FuseRead::CommandProbe)?;
    let repeated = read(address, FuseRead::RepeatFuse)?;
    if first_probe == second_probe {
        return Err(SlotError::UnverifiableFuse {
            ccd,
            probe: first_probe,
        });
    }
    if fuse != repeated {
        return Err(SlotError::FuseDisagree {
            ccd,
            first: fuse,
            second: repeated,
        });
    }
    Ok(fuse.to_le_bytes()[0])
}

/// Maps every core to its CCD and slot from the topology and the CCD fuses.
///
/// Refuses before any SMN read when the identity is ambiguous or a raw core id is outside 0–15.
pub(crate) fn map(cpus: &[RawCpu], smn: &mut SmnRead<'_>) -> Result<SlotMap, SlotError> {
    let mut cores = topology::group(cpus)?;
    cores.sort_by_key(|core| (core.ccd, core.core_id));
    let ccds: Vec<(CcdId, Vec<i64>)> = cores
        .chunk_by(|a, b| a.ccd == b.ccd)
        .map(|group| {
            (
                group[0].ccd,
                group.iter().map(|core| core.core_id).collect(),
            )
        })
        .collect();
    let outside: Vec<(CcdId, Vec<i64>)> = ccds
        .iter()
        .filter_map(|(ccd, ids)| {
            let bad: Vec<i64> = ids
                .iter()
                .copied()
                .filter(|id| CoreId::new(*id).is_err())
                .collect();
            (!bad.is_empty()).then_some((*ccd, bad))
        })
        .collect();
    if !outside.is_empty() {
        return Err(SlotError::OutOfRange(outside));
    }
    let mut locations = [None; PER_CORE_MAX];
    let mut details = Vec::new();
    for (ccd, ids) in &ccds {
        let ccd = *ccd;
        let mask = read_disabled_mask(ccd, smn)?;
        if mask != 0 {
            return Err(SlotError::Harvested { ccd, mask });
        }
        if ids.len() != SLOTS_PER_CCD {
            return Err(SlotError::CoreCount {
                ccd,
                mask,
                cores: ids.len(),
            });
        }
        let members: Vec<CoreId> = ids.iter().filter_map(|id| CoreId::new(*id).ok()).collect();
        let misplaced: Vec<Misplaced> = members
            .iter()
            .enumerate()
            .filter(|(slot, core)| core.index() % SLOTS_PER_CCD != *slot)
            .map(|(slot, core)| Misplaced {
                core: *core,
                slot,
                want: core.index() % SLOTS_PER_CCD,
            })
            .collect();
        if !misplaced.is_empty() {
            return Err(SlotError::SlotOrder { ccd, misplaced });
        }
        for (slot, core) in (0_u8..).zip(&members) {
            locations[core.index()] = Some(Location { ccd, slot });
        }
        if let (Some(first), Some(last)) = (members.first(), members.last()) {
            let mut detail = String::new();
            let _ = write!(
                detail,
                "CCD{} fuse {mask:#04x}: cores {first:02}-{last:02} on slots 0-7",
                ccd.0
            );
            details.push(detail);
        }
    }
    Ok(SlotMap {
        locations,
        detail: details.join("; "),
    })
}
