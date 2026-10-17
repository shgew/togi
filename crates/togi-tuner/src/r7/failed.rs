use std::collections::BTreeMap;

use togi_journal::Seq;
use togi_machine::{CcdId, CoreId, CoreSet, PerCore};

use super::escalation::Backoffs;
use super::measurements::{Measurement, Measurements};
use super::order::RequestOrder;
use crate::failures::Pending;
use crate::{ClassKey, Count, LoadKey, Profile, Topology};

/// What a trial reported about requests, clocks and stalls.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Telemetry<'a> {
    pub(crate) requests: &'a PerCore<f64>,
    pub(crate) top: CoreSet,
    pub(crate) clocks: &'a BTreeMap<CcdId, u32>,
    pub(crate) stalled: Option<CoreId>,
}

/// A conclusive R7 trial's end: where in the journal, what ran and what it reported.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ended<'a> {
    pub(crate) seq: Seq,
    pub(crate) class: ClassKey,
    pub(crate) profile: &'a Profile,
    pub(crate) telemetry: Telemetry<'a>,
    pub(crate) named: Option<CoreId>,
}

/// The request order a trial explains: its own requests, else the newest measurements folded
/// before it. Nothing re-derives it afterwards.
pub(crate) fn order_at(
    measurements: &Measurements,
    topology: &Topology,
    class: ClassKey,
    profile: &Profile,
    end: Seq,
    requests: &PerCore<f64>,
) -> RequestOrder {
    if requests.is_empty() {
        RequestOrder::derive(measurements, topology, class.load(), profile)
    } else {
        let own = Measurement::new(end, *profile, *requests);
        RequestOrder::measured(topology, class.load(), &own)
    }
}

/// The top requesters a trial recorded, else the ones its order gives.
pub(crate) fn effective_top(recorded: CoreSet, order: &RequestOrder) -> CoreSet {
    if recorded.is_empty() {
        order.top()
    } else {
        recorded
    }
}

/// A failed multi-core R7 trial as it ended: what attribution reads, frozen when the trial's
/// failure folds so that a later measurement, a later pass or a later reset never reinterprets
/// it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FailedTrial {
    pub(crate) end: Seq,
    pub(crate) stalled: Option<CoreId>,
    pub(crate) recorded_top: CoreSet,
    pub(crate) clocks: BTreeMap<CcdId, i64>,
    pub(crate) order: RequestOrder,
    named_lane: Option<(CoreId, RequestOrder)>,
    pub(crate) backoffs: Backoffs,
    pub(crate) located_loaded: Option<Seq>,
    /// The passes of the load, as the failure folds, that hold every loaded core at least as
    /// deep with an unloaded core shallower ([`crate::Evidence::loaded_cover`]): the count of
    /// them and the earliest ones, up to the supported trial bound. Frozen at the fold; a later
    /// pass, invalidation, reset or configuration change never alters it.
    pub(crate) cover: Count,
}

/// The measurement a failing request came from, when one covered the core.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FailingRequest {
    pub(crate) volts: Option<f64>,
    pub(crate) sources: Vec<Seq>,
}

impl FailedTrial {
    pub(crate) fn freeze(
        measurements: &Measurements,
        topology: &Topology,
        ended: Ended<'_>,
        order: RequestOrder,
    ) -> Self {
        let Ended {
            seq: end,
            class,
            profile,
            telemetry,
            named,
        } = ended;
        let named_lane = named
            .filter(|&core| topology.contains(core))
            .and_then(|core| {
                let lane_in_order = class.loaded().contains(core)
                    && (telemetry.requests.is_empty() || telemetry.requests.get(core).is_some());
                (!lane_in_order).then(|| {
                    let load = LoadKey::new(class.workload(), std::iter::once(core).collect());
                    (
                        core,
                        RequestOrder::derive(measurements, topology, load, profile),
                    )
                })
            });
        Self {
            end,
            stalled: telemetry.stalled,
            recorded_top: telemetry.top,
            clocks: telemetry
                .clocks
                .iter()
                .map(|(&ccd, &mhz)| (ccd, i64::from(mhz)))
                .collect(),
            order,
            named_lane,
            backoffs: Backoffs::default(),
            located_loaded: None,
            cover: Count::default(),
        }
    }

    pub(crate) fn top(&self) -> CoreSet {
        effective_top(self.recorded_top, &self.order)
    }

    /// The earliest `needed` of the frozen passes showing the loaded cores pass on their own as
    /// deep as they failed, once the snapshot holds that many. A `needed` the snapshot cannot
    /// serve, as a larger configuration after a reload may ask, finds none.
    pub(crate) fn covered(&self, needed: std::num::NonZeroU16) -> Option<&[Seq]> {
        self.cover.witnesses.get(..usize::from(needed.get()))
    }

    /// The failing request of `core`: the measurement the trial's order took it from, else the
    /// one the named core's own order found. Unmeasured cores carry the sources that did not
    /// cover them.
    pub(crate) fn failing_request(&self, core: CoreId) -> FailingRequest {
        let order = match &self.named_lane {
            Some((named, lane)) if *named == core && self.order.volts(core).is_none() => lane,
            _ => &self.order,
        };
        FailingRequest {
            volts: order.volts(core),
            sources: order.sources().to_vec(),
        }
    }
}

/// A multi-core R7 failure the tuner still owes a response, with the cores already answered.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenFailure {
    pub(crate) seq: Seq,
    pub(crate) pending: Pending,
    pub(crate) r7: FailedTrial,
    pub(crate) handled: CoreSet,
}
