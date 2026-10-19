use togi_machine::{CcdId, CoreId, CoreSet, PerCore, TargetOffset};
use togi_requests::{VOLTS_PER_COUNT, groups, top};

use super::measurements::{Measurement, Measurements};
use togi_journal::Seq;

use crate::{Depth, LoadKey, Profile, Ranking, Topology};

/// Where one loaded core's request came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Lane {
    /// No measurement covers the core: its offset orders it, shallowest first, and stands in for
    /// a request it must never be reported as.
    #[default]
    OffsetProxy,
    Measured {
        source: Seq,
    },
}

/// The request groups of the loaded cores on one CCD, highest first.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CcdOrder {
    ccd: CcdId,
    loaded: CoreSet,
    groups: Vec<CoreSet>,
    rail: f64,
}

/// The first group of a CCD holding a core that can still move.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Movable<'a> {
    /// The movable members of the group.
    pub(crate) cores: CoreSet,
    /// The groups above it, every core of which sits at CO 0.
    pub(crate) skipped: &'a [CoreSet],
    /// The CCD's highest request in the order, the one a step-down must rise above.
    pub(crate) rail: f64,
}

#[cfg(test)]
impl Movable<'_> {
    pub(crate) fn stepped_down(&self) -> bool {
        !self.skipped.is_empty()
    }
}

impl CcdOrder {
    #[cfg(test)]
    pub(crate) fn ccd(&self) -> CcdId {
        self.ccd
    }

    pub(crate) fn loaded(&self) -> CoreSet {
        self.loaded
    }

    pub(crate) fn groups(&self) -> &[CoreSet] {
        &self.groups
    }

    pub(crate) fn rail(&self) -> f64 {
        self.rail
    }

    pub(crate) fn top(&self) -> CoreSet {
        self.groups.first().copied().unwrap_or_default()
    }

    #[cfg(test)]
    /// The first group with a loaded core off CO 0 in `profile`, reduced to those cores. A mixed
    /// tie at the top is no step-down: its nonzero members are the group.
    pub(crate) fn first_movable(&self, profile: &Profile) -> Option<Movable<'_>> {
        self.groups.iter().enumerate().find_map(|(at, group)| {
            let cores: CoreSet = group
                .iter()
                .filter(|&core| profile.get(core).is_some_and(|at| at.signed() != 0))
                .collect();
            (!cores.is_empty()).then_some(Movable {
                cores,
                skipped: &self.groups[..at],
                rail: self.rail,
            })
        })
    }
}

/// The request order of a load at one profile: every loaded core's request or its offset
/// stand-in, the groups per CCD, and the measurements that supplied it. It is derived once, when
/// the trial it explains ends, and kept: a later measurement never reinterprets an old outcome.
///
/// The fold derives it at the `trial.end` or `trial.carried` of the failed trial, before that
/// event's own measurement is recorded. A trial that recorded its own requests uses
/// [`RequestOrder::measured`], which takes precedence; any other uses [`RequestOrder::derive`],
/// which sees only the measurements folded so far, the ones available as of that trial. The
/// result is stored in the pending failure, and nothing re-derives it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequestOrder {
    values: PerCore<f64>,
    lanes: PerCore<Lane>,
    sources: Vec<Seq>,
    ccds: Vec<CcdOrder>,
}

impl RequestOrder {
    /// The order at `profile` from the newest measurement of exactly `load`, else on each CCD
    /// the newest measurement of that CCD's full part, else the offsets. A measurement shifts
    /// each core by one count per offset step between its profile and `profile`. A core the
    /// chosen measurement has no lane for is ordered by its offset.
    pub(crate) fn derive(
        measurements: &Measurements,
        topology: &Topology,
        load: LoadKey,
        profile: &Profile,
    ) -> Self {
        let exact = measurements.newest(load);
        Self::build(topology, load, profile, |part| {
            exact.or_else(|| measurements.newest(LoadKey::new(load.workload, part)))
        })
    }

    /// The order a trial's own measurement gives, at the profile the trial ran.
    pub(crate) fn measured(topology: &Topology, load: LoadKey, own: &Measurement) -> Self {
        Self::build(topology, load, own.profile(), |_| Some(own))
    }

    fn build<'a>(
        topology: &Topology,
        load: LoadKey,
        profile: &Profile,
        source: impl Fn(CoreSet) -> Option<&'a Measurement>,
    ) -> Self {
        let mut values = PerCore::default();
        let mut lanes = PerCore::default();
        let mut sources = Vec::new();
        let mut ccds = Vec::new();
        for &(ccd, part) in topology.ccds() {
            let loaded: CoreSet = part
                .iter()
                .filter(|&core| load.loaded.contains(core))
                .collect();
            if loaded.is_empty() {
                continue;
            }
            let measurement = source(part);
            let mut here = PerCore::default();
            for core in loaded {
                let Some(applied) = profile.get(core).map(TargetOffset::signed) else {
                    continue;
                };
                let measured = measurement.and_then(|m| {
                    let shift = f64::from(applied - m.profile().get(core)?.signed());
                    m.request(core)
                        .map(|request| (m.seq(), request + VOLTS_PER_COUNT * shift))
                });
                let (lane, value) = match measured {
                    Some((source, value)) => {
                        sources.push(source);
                        (Lane::Measured { source }, value)
                    }
                    None => (Lane::OffsetProxy, VOLTS_PER_COUNT * f64::from(applied)),
                };
                values.set(core, value);
                lanes.set(core, lane);
                here.set(core, value);
            }
            if let Some(rail) = top(&here) {
                ccds.push(CcdOrder {
                    ccd,
                    loaded,
                    groups: groups(&here),
                    rail,
                });
            }
        }
        sources.sort_unstable();
        sources.dedup();
        Self {
            values,
            lanes,
            sources,
            ccds,
        }
    }

    /// The measurements that supplied a lane, ascending, each once. Empty when every core is
    /// ordered by its offset: the order then says it fell back to offsets.
    pub(crate) fn sources(&self) -> &[Seq] {
        &self.sources
    }

    #[cfg(test)]
    pub(crate) fn is_offset_only(&self) -> bool {
        self.sources.is_empty()
    }

    pub(crate) fn lane(&self, core: CoreId) -> Option<Lane> {
        self.lanes.get(core)
    }

    /// The core's request in volts, only when a measurement supplied it.
    pub(crate) fn volts(&self, core: CoreId) -> Option<f64> {
        match self.lane(core)? {
            Lane::Measured { .. } => self.values.get(core),
            Lane::OffsetProxy => None,
        }
    }

    /// The core's request or offset stand-in, in volts.
    pub(crate) fn value(&self, core: CoreId) -> Option<f64> {
        self.values.get(core)
    }

    /// The loaded cores ordered by their offsets.
    pub(crate) fn proxy(&self) -> CoreSet {
        self.lanes
            .iter()
            .filter(|&(_, lane)| lane == Lane::OffsetProxy)
            .map(|(core, _)| core)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn ccds(&self) -> &[CcdOrder] {
        &self.ccds
    }

    pub(crate) fn ccd(&self, ccd: CcdId) -> Option<&CcdOrder> {
        self.ccds.iter().find(|order| order.ccd == ccd)
    }

    /// The top requesters: the first group of every loaded CCD.
    pub(crate) fn top(&self) -> CoreSet {
        self.ccds.iter().flat_map(CcdOrder::top).collect()
    }
}

/// The member of `group` a backoff moves: the one the ranking prefers least, the larger core id
/// when the ranking does not tell two apart.
pub(crate) fn lowest_ranked(group: CoreSet, ranking: &Ranking) -> Option<CoreId> {
    group.iter().max_by_key(|&core| {
        (
            ranking.order().iter().position(|&ranked| ranked == core),
            core,
        )
    })
}
