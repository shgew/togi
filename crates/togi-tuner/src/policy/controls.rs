//! Stock controls: the all-zero matrix a first night and every firmware change must pass, the CO-0 rerun a
//! nonzero failure on a zero-offset core owes, and the dead end a conclusive all-zero failure is.

use std::collections::HashSet;

use crate::UnitProfile;

use super::State;
use super::books::Exposure;
use super::ledger::{Sig, cell_of_class};
use super::night_query::NotReady;
use crate::night::{Cell, ControlCause, Layout, Reason, Stock, Work, control_cells};

impl State {
    /// The durable ordinal of the controls under the current evidence context: how many firmware or content
    /// changes came before it, so each change rotates the sampled entries and positions.
    pub(super) fn control_ordinal(&self) -> Result<u32, NotReady> {
        self.machine.firmware.as_ref().ok_or(NotReady::NoFirmware)?;
        Ok(self.machine.evidence_changes)
    }

    /// The matrix cell that produced an exposure, rebuilt from its recorded class. The class carries
    /// everything the cell is, so the answer does not depend on today's partial chains.
    pub(super) fn exposure_cell(&self, record: &Exposure) -> Result<Cell, NotReady> {
        let topology = self.machine.topology.as_ref().ok_or(NotReady::NoSession)?;
        cell_of_class(&record.observed.class, topology, &self.machine.cores)
            .ok_or(NotReady::UnmatchedCell)
    }

    /// Where stock controls stand.
    pub(super) fn stock(&self, layout: &Layout<'_>) -> Result<Stock, NotReady> {
        let firmware = self.machine.firmware.as_ref().ok_or(NotReady::NoFirmware)?;
        if let Some(record) = self
            .current_groups()
            .filter_map(|group| group.zero_failure)
            .filter_map(|index| self.ledger.records.get(index))
            .min_by_key(|record| record.seq)
        {
            return Ok(Stock::Unstable {
                class: self.exposure_cell(record)?,
            });
        }
        let zero = UnitProfile::zero(&crate::Units::coarse(layout.topology));
        let ordinal = self.control_ordinal()?;
        let matrix = control_cells(layout, &zero, ordinal)?;
        let passed: HashSet<Sig> = self
            .current_groups()
            .flat_map(|group| group.control_passed.iter().cloned())
            .collect();
        let cells: Vec<Cell> = matrix
            .fresh()
            .filter(|cell| !passed.contains(&Sig::of_cell(cell)))
            .copied()
            .collect();
        if !cells.is_empty() {
            let cause = if self.machine.contexts.first() != Some(firmware) {
                ControlCause::FirmwareChange
            } else if self.ledger.records.is_empty() {
                ControlCause::FirstNight
            } else {
                ControlCause::EvidenceChange
            };
            return Ok(Stock::Pending {
                cause,
                work: Work {
                    profile: zero,
                    cells,
                    reason: Reason::StockControls { cause },
                },
            });
        }
        Ok(Stock::Cleared {
            rerun: self.pending_rerun()?,
        })
    }

    /// The oldest nonzero-profile failure that named a core at CO 0 and has no all-zero rerun since.
    fn pending_rerun(&self) -> Result<Option<Work>, NotReady> {
        let mut candidates: Vec<&Exposure> = self
            .current_groups()
            .flat_map(|group| group.zero_core_failures.iter())
            .filter_map(|&index| self.ledger.records.get(index))
            .collect();
        candidates.sort_by_key(|record| record.seq);
        for record in candidates {
            let observed = &record.observed;
            let sig = Sig::of_class(&observed.class);
            let rerun = self.current_groups().any(|group| {
                group
                    .zero_control_latest
                    .get(&sig)
                    .is_some_and(|&later| later > record.seq)
            });
            if !rerun {
                let cell = self.exposure_cell(record)?;
                return Ok(Some(Work::co_zero_rerun(
                    &cell,
                    &UnitProfile::of_ccd(observed.profile.ccd()),
                )));
            }
        }
        Ok(None)
    }
}
