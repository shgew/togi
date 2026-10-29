use std::collections::BTreeMap;

use togi_journal::Seq;
use togi_machine::{CcdId, CoreId, CoreInfo, CoreSet, CpuId, TargetOffset};
use togi_tuner::{HonestChain, Topology};
use togi_tuner::unit::{UnitId, UnitProfile, Units};

pub(crate) fn ccd(id: u8) -> CcdId {
    CcdId::try_from(id).unwrap()
}

pub(crate) fn unit(id: u8) -> UnitId {
    UnitId::Ccd(ccd(id))
}

pub(crate) fn core(id: u8) -> CoreId {
    CoreId::new(i64::from(id)).unwrap()
}

pub(crate) fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

pub(crate) fn offset(counts: i64) -> TargetOffset {
    TargetOffset::new(counts).unwrap()
}

pub(crate) fn seq(value: u64) -> Seq {
    Seq::new(value).unwrap()
}

/// `ccds` CCDs of `per_ccd` cores each; core `k` has logical CPUs `k` and `k + 32`.
pub(crate) struct Machine {
    pub(crate) topology: Topology,
    pub(crate) cores: Vec<CoreInfo>,
    pub(crate) partials: BTreeMap<CcdId, HonestChain>,
}

pub(crate) fn machine(ccds: u8, per_ccd: u8) -> Machine {
    let cores: Vec<CoreInfo> = (0..ccds)
        .flat_map(|c| {
            (0..per_ccd).map(move |i| {
                let id = c * per_ccd + i;
                CoreInfo {
                    core: core(id),
                    ccd: ccd(c),
                    cpus: vec![
                        CpuId::new(i64::from(id)).unwrap(),
                        CpuId::new(i64::from(id) + 32).unwrap(),
                    ],
                }
            })
        })
        .collect();
    Machine {
        topology: Topology::new(&cores).unwrap(),
        cores,
        partials: BTreeMap::new(),
    }
}

pub(crate) fn profile(offsets: &[i64]) -> UnitProfile {
    let machine = machine(u8::try_from(offsets.len()).unwrap(), 1);
    let zero = UnitProfile::zero(&Units::coarse(&machine.topology));
    offsets
        .iter()
        .zip(0..)
        .fold(zero, |profile, (&counts, id)| {
            profile.with(unit(id), offset(counts))
        })
}

impl Machine {
    pub(crate) fn layout(&self) -> togi_tuner::night::Layout<'_> {
        togi_tuner::night::Layout {
            topology: &self.topology,
            cores: &self.cores,
            partials: &self.partials,
        }
    }
}
