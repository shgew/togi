//! The exposure aggregates every query reads. An exposure folds into the group that shares its firmware
//! context, epoch and workload contents; a query then asks only the groups that are current, so its cost
//! follows the number of classes and not the length of the journal.

use std::collections::{BTreeMap, HashMap};

use togi_journal::{ExposureObserved, Outcome, Seq, Tested, WorkloadContent};
use togi_machine::{BiosContext, CcdId, CoreInfo, TargetOffset};

use super::State;
use super::ledger::{Sig, failures_on, is_adequate};
use crate::EVIDENCE_EPOCH;

/// What makes exposures comparable evidence: one firmware context, one epoch, one set of contents.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct GroupKey {
    pub(super) firmware: BiosContext,
    pub(super) epoch: u32,
    pub(super) contents: Vec<WorkloadContent>,
}

impl GroupKey {
    /// Membership in this evidence context, independent of a trial's profile or purpose.
    pub(super) fn matches(&self, observed: &ExposureObserved) -> bool {
        self.firmware == observed.firmware
            && self.epoch == observed.epoch
            && self.contents == observed.class.workloads
    }
}

/// Everything the exposures of one group say.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Group {
    pub(super) key: GroupKey,
    /// The first failure at the all-zero profile, as a ledger index.
    pub(super) zero_failure: Option<usize>,
    /// The adequate isolated passes of a class on a CCD, with their offsets.
    pub(super) isolated_passes: HashMap<(CcdId, Sig), Vec<(TargetOffset, Seq)>>,
    /// The failures that count against a CCD, with the offset they count at.
    pub(super) scoped_failures: BTreeMap<CcdId, Vec<(Seq, TargetOffset)>>,
}

/// The CCD-local offset of an isolated or single-CCD exact observation.
pub(super) fn ccd_scope(
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
        // multi-CCD exact trial is profile-level and has no CCD-local offset.
        Tested::Exact {} => {
            let on = &observed.class.cores;
            let confined = !on.is_empty()
                && on.iter().all(|core| {
                    cores
                        .iter()
                        .any(|info| info.core == core && info.ccd == ccd)
                });
            if confined {
                observed.profile.ccd().get(ccd)
            } else {
                None
            }
        }
    }
}

impl Group {
    fn new(key: GroupKey) -> Self {
        Self {
            key,
            zero_failure: None,
            isolated_passes: HashMap::new(),
            scoped_failures: BTreeMap::new(),
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
        if let Tested::Isolated { ccd, offset } = observed.tested
            && is_adequate(observed, cores)
        {
            self.isolated_passes
                .entry((ccd, Sig::of_class(&observed.class)))
                .or_default()
                .push((offset, seq));
        }
        if observed.profile.ccd().is_zero()
            && observed.outcome == Outcome::Failure
            && !observed.failures.is_empty()
            && self.zero_failure.is_none()
        {
            self.zero_failure = Some(index);
        }
        for &ccd in ccds {
            let Some(offset) = ccd_scope(cores, observed, ccd) else {
                continue;
            };
            let failures = failures_on(observed, ccd, cores);
            if failures > 0 {
                self.scoped_failures
                    .entry(ccd)
                    .or_default()
                    .push((seq, offset));
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
        self.is_current_evidence(&group.key.firmware, group.key.epoch, &group.key.contents)
    }

    /// Whether an exposure holds under the current firmware, epoch and workload contents.
    pub(super) fn is_current_exposure(&self, observed: &ExposureObserved) -> bool {
        self.is_current_evidence(
            &observed.firmware,
            observed.epoch,
            &observed.class.workloads,
        )
    }

    fn is_current_evidence(
        &self,
        firmware: &BiosContext,
        epoch: u32,
        contents: &[WorkloadContent],
    ) -> bool {
        self.machine.firmware.as_ref() == Some(firmware)
            && epoch == EVIDENCE_EPOCH
            && contents.iter().all(|content| {
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
