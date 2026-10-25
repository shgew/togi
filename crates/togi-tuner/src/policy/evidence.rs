//! The exposure aggregates every query reads. An exposure folds into the group that shares its firmware
//! context, epoch and workload contents; a query then asks only the groups that are current, so its cost
//! follows the number of classes and not the length of the journal.

use std::collections::{BTreeMap, HashMap, HashSet};

use togi_journal::{Activity, ExposureObserved, Outcome, Seq, Tested, WorkloadContent};
use togi_machine::{BiosContext, CcdId, CoreInfo, TargetOffset};

use super::State;
use super::ledger::{Sig, cap_purpose, failures_on, is_adequate, saturating_u32};
use crate::EVIDENCE_EPOCH;
use crate::cap::{self, Hours};

/// What makes exposures comparable evidence: one firmware context, one epoch, one set of contents.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct GroupKey {
    pub(super) firmware: BiosContext,
    pub(super) epoch: u32,
    pub(super) contents: Vec<WorkloadContent>,
}

/// One cap cell of a class, merged over the exposures that share its offset, purpose and adequacy.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct CapCell {
    pub(super) offset: TargetOffset,
    pub(super) purpose: cap::Purpose,
    pub(super) exposure: cap::Exposure,
    pub(super) failures: u32,
    pub(super) hours: f64,
}

/// One class's cap input for one CCD.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct ClassCap {
    pub(super) sig: Sig,
    pub(super) class: togi_journal::ExposureClass,
    /// The first exposure of the class that counted for the CCD: it orders the classes.
    pub(super) first: Seq,
    pub(super) cells: Vec<CapCell>,
    /// Eligible seconds of the class's mapping exposure per offset.
    pub(super) mapping_secs: BTreeMap<TargetOffset, u64>,
    pub(super) latest_mapping: Option<Seq>,
}

/// Everything the exposures of one group say.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Group {
    pub(super) key: GroupKey,
    #[serde(with = "super::wire::pairs")]
    pub(super) cap: BTreeMap<CcdId, Vec<ClassCap>>,
    /// The classes with an adequate mapping exposure at an offset of a CCD.
    #[serde(with = "super::wire::pairs_of_sets")]
    pub(super) mapping_covered: HashMap<(CcdId, TargetOffset), HashSet<Sig>>,
    /// The classes an adequate all-zero control passed.
    #[serde(serialize_with = "togi_machine::serde_pairs::serialize_set")]
    pub(super) control_passed: HashSet<Sig>,
    /// The first failure at the all-zero profile, as a ledger index.
    pub(super) zero_failure: Option<usize>,
    /// The ledger indexes of nonzero-profile failures that named a core at CO 0.
    pub(super) zero_core_failures: Vec<usize>,
    /// The latest conclusive all-zero control of each class: an adequate pass or a failure.
    #[serde(with = "togi_machine::serde_pairs")]
    pub(super) zero_control_latest: HashMap<Sig, Seq>,
    /// The adequate isolated passes of a class on a CCD, with their offsets.
    #[serde(with = "togi_machine::serde_pairs")]
    pub(super) isolated_passes: HashMap<(CcdId, Sig), Vec<(TargetOffset, Seq)>>,
    /// The failures that count against a CCD, with the offset they count at.
    #[serde(with = "super::wire::pairs")]
    pub(super) scoped_failures: BTreeMap<CcdId, Vec<(Seq, TargetOffset)>>,
    /// The latest exposure that counts for a CCD's cap.
    #[serde(with = "super::wire::pairs")]
    pub(super) latest_scoped: BTreeMap<CcdId, Seq>,
}

/// Whether a recorded exposure counts toward `ccd`'s cap, and at which offset.
pub(super) fn cap_scope(
    cores: &[CoreInfo],
    observed: &ExposureObserved,
    ccd: CcdId,
) -> Option<TargetOffset> {
    match observed.tested {
        Tested::Isolated {
            ccd: tested,
            offset,
        } => (tested == ccd).then_some(offset),
        // An exact trial confined to one CCD is that CCD's evidence at the profile's offset there; a
        // multi-CCD exact trial is profile-level and stays out of every CCD's cap.
        Tested::Exact {} => {
            let on = &observed.class.cores;
            let confined = !on.is_empty()
                && on.iter().all(|core| {
                    cores
                        .iter()
                        .any(|info| info.core == core && info.ccd == ccd)
                });
            if confined {
                observed.profile.ccd.get(ccd)
            } else {
                None
            }
        }
    }
}

const SECS_PER_HOUR: f64 = 3600.0;

impl Group {
    fn new(key: GroupKey) -> Self {
        Self {
            key,
            cap: BTreeMap::new(),
            mapping_covered: HashMap::new(),
            control_passed: HashSet::new(),
            zero_failure: None,
            zero_core_failures: Vec::new(),
            zero_control_latest: HashMap::new(),
            isolated_passes: HashMap::new(),
            scoped_failures: BTreeMap::new(),
            latest_scoped: BTreeMap::new(),
        }
    }

    /// Folds one exposure, the `index`th of the ledger, into the aggregates.
    fn add(
        &mut self,
        index: usize,
        seq: Seq,
        observed: &ExposureObserved,
        cores: &[CoreInfo],
        ccds: &[CcdId],
    ) {
        let sig = Sig::of_class(&observed.class);
        let adequate = is_adequate(observed, cores);
        let zero = observed.profile.ccd.is_zero();
        if let Tested::Isolated { ccd, offset } = observed.tested {
            if observed.activity == Activity::Mapping && adequate {
                self.mapping_covered
                    .entry((ccd, offset))
                    .or_default()
                    .insert(sig.clone());
            }
            if adequate {
                self.isolated_passes
                    .entry((ccd, sig.clone()))
                    .or_default()
                    .push((offset, seq));
            }
        }
        if zero {
            if observed.outcome == Outcome::Failure
                && !observed.failures.is_empty()
                && self.zero_failure.is_none()
            {
                self.zero_failure = Some(index);
            }
            if observed.activity == Activity::Control {
                let conclusive = match observed.outcome {
                    Outcome::Pass => adequate,
                    Outcome::Failure => !observed.failures.is_empty(),
                    Outcome::Inconclusive => false,
                };
                if conclusive {
                    self.zero_control_latest.insert(sig.clone(), seq);
                }
                if observed.tested == (Tested::Exact {}) && adequate {
                    self.control_passed.insert(sig.clone());
                }
            }
        } else if observed.outcome == Outcome::Failure
            && observed.failures.iter().any(|failure| {
                failure.core.is_some_and(|core| {
                    cores
                        .iter()
                        .find(|info| info.core == core)
                        .and_then(|info| observed.profile.ccd.get(info.ccd))
                        .is_some_and(|offset| offset == TargetOffset::ZERO)
                })
            })
        {
            self.zero_core_failures.push(index);
        }
        for &ccd in ccds {
            let Some(offset) = cap_scope(cores, observed, ccd) else {
                continue;
            };
            self.latest_scoped.insert(ccd, seq);
            let failures = failures_on(observed, ccd, cores);
            if failures > 0 {
                self.scoped_failures
                    .entry(ccd)
                    .or_default()
                    .push((seq, offset));
            }
            let Some(purpose) = cap_purpose(observed.activity) else {
                continue;
            };
            let classes = self.cap.entry(ccd).or_default();
            let at = if let Some(at) = classes.iter().position(|known| known.sig == sig) {
                at
            } else {
                classes.push(ClassCap {
                    sig: sig.clone(),
                    class: observed.class.clone(),
                    first: seq,
                    cells: Vec::new(),
                    mapping_secs: BTreeMap::new(),
                    latest_mapping: None,
                });
                classes.len() - 1
            };
            let class = &mut classes[at];
            let hours = Hours::new(f64::from(saturating_u32(observed.eligible_s)) / SECS_PER_HOUR)
                .unwrap_or(Hours::ZERO)
                .get();
            let exposure = if adequate {
                cap::Exposure::Adequate
            } else {
                cap::Exposure::Partial
            };
            match class.cells.iter_mut().find(|cell| {
                cell.offset == offset && cell.purpose == purpose && cell.exposure == exposure
            }) {
                Some(cell) => {
                    cell.failures = cell.failures.saturating_add(failures);
                    cell.hours = (cell.hours + hours).min(f64::MAX);
                }
                None => class.cells.push(CapCell {
                    offset,
                    purpose,
                    exposure,
                    failures,
                    hours,
                }),
            }
            if purpose == cap::Purpose::Mapping {
                *class.mapping_secs.entry(offset).or_default() += observed.eligible_s;
                class.latest_mapping = Some(seq);
            }
        }
    }
}

impl State {
    /// Folds an exposure into its group, creating the group on first sight.
    pub(super) fn aggregate(&mut self, index: usize, seq: Seq, observed: &ExposureObserved) {
        let key = GroupKey {
            firmware: observed.firmware.clone(),
            epoch: observed.epoch,
            contents: observed.class.workloads.clone(),
        };
        let at = if let Some(at) = self.ledger.groups.iter().position(|group| group.key == key) {
            at
        } else {
            self.ledger.groups.push(Group::new(key));
            self.ledger.groups.len() - 1
        };
        let ccds = self.ccds();
        let cores = &self.machine.cores;
        if let Some(group) = self.ledger.groups.get_mut(at) {
            group.add(index, seq, observed, cores, &ccds);
        }
    }

    /// Whether a group holds under the current firmware, epoch and workload contents.
    pub(super) fn is_current_group(&self, group: &Group) -> bool {
        self.machine.firmware.as_ref() == Some(&group.key.firmware)
            && group.key.epoch == EVIDENCE_EPOCH
            && group.key.contents.iter().all(|content| {
                self.machine.contents.get(&content.workload.runs()) == Some(&content.content)
            })
    }

    /// The groups that hold under the current firmware, epoch and workload contents.
    pub(super) fn current_groups(&self) -> impl Iterator<Item = &Group> {
        self.ledger
            .groups
            .iter()
            .filter(|group| self.is_current_group(group))
    }
}
