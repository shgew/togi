use std::collections::HashMap;
use std::collections::hash_map::Entry;

use togi_machine::{CoreId, PerCore};

use togi_journal::Seq;

use crate::{LoadKey, Profile};

/// The request telemetry of one R7 trial: the profile it ran at and each loaded core's request.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Measurement {
    seq: Seq,
    profile: Profile,
    requests: PerCore<f64>,
}

impl Measurement {
    pub(crate) fn new(seq: Seq, profile: Profile, requests: PerCore<f64>) -> Self {
        Self {
            seq,
            profile,
            requests,
        }
    }

    pub(crate) fn seq(&self) -> Seq {
        self.seq
    }

    pub(crate) fn profile(&self) -> &Profile {
        &self.profile
    }

    pub(crate) fn request(&self, core: CoreId) -> Option<f64> {
        self.requests.get(core)
    }
}

/// The newest request measurement of every load. A load answers with its newest measurement
/// whatever the duration or outcome of the trial that took it, so older ones are dropped as
/// newer ones arrive. The newest full part of a CCD is the measurement of the load that is the
/// CCD's cores.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Measurements {
    newest: HashMap<LoadKey, Measurement>,
}

impl Measurements {
    /// Keeps `measurement` when it is the newest of its load. A measurement without a request
    /// lane is no measurement. Returns whether it was kept.
    pub(crate) fn record(&mut self, load: LoadKey, measurement: Measurement) -> bool {
        if measurement.requests.is_empty() {
            return false;
        }
        match self.newest.entry(load) {
            Entry::Vacant(slot) => {
                slot.insert(measurement);
                true
            }
            Entry::Occupied(mut slot) => {
                let newer = measurement.seq > slot.get().seq;
                if newer {
                    slot.insert(measurement);
                }
                newer
            }
        }
    }

    pub(crate) fn newest(&self, load: LoadKey) -> Option<&Measurement> {
        self.newest.get(&load)
    }

    /// Drops every measurement whose load includes `core`. A measurement of a load without it
    /// stays, whichever core a failure of that load named.
    pub(crate) fn reset_core(&mut self, core: CoreId) {
        self.newest.retain(|load, _| !load.loaded.contains(core));
    }
}
