//! Stock controls: the all-zero matrix a first night and every firmware change must pass, the CO-0 rerun a
//! nonzero failure on a zero-offset core owes, and the dead end a conclusive all-zero failure is.

use std::collections::HashSet;

use togi_journal::{Queue, Tested};
use togi_machine::CcdProfile;

use super::State;
use super::books::Exposure;
use super::ledger::Sig;
use super::night_query::NotReady;
use crate::night::{
    Cell, ControlCause, Layout, Matrix, Reason, Reuse, Stock, Work, control_cells, mapping_cells,
    qualification_cells, reproduction_cells,
};

impl State {
    /// The durable ordinal of the controls under the current evidence context: how many firmware or content
    /// changes came before it, so each change rotates the sampled entries and positions.
    pub(super) fn control_ordinal(&self) -> Result<u32, NotReady> {
        self.machine.firmware.as_ref().ok_or(NotReady::NoFirmware)?;
        Ok(self.machine.evidence_changes)
    }

    /// The matrix cell that produced an exposure, recomputed from its task.
    pub(super) fn exposure_cell(layout: &Layout<'_>, record: &Exposure) -> Result<Cell, NotReady> {
        let task = record.task.ok_or(NotReady::UnmatchedCell)?;
        let profile = &record.observed.profile.ccd;
        let sig = Sig::of_class(&record.observed.class);
        let matrices: Vec<Matrix> = match task.queue {
            Queue::Control => vec![control_cells(layout, profile, task.ordinal)?],
            Queue::Qualification | Queue::Requalification => vec![qualification_cells(
                layout,
                profile,
                task.ordinal,
                &Reuse::AllFresh,
            )?],
            Queue::Mapping => match record.observed.tested {
                Tested::Isolated { ccd, offset } => {
                    vec![mapping_cells(layout, ccd, offset, task.ordinal)?]
                }
                Tested::Exact {} => Vec::new(),
            },
            Queue::Reproduction => reproduction_cells(layout, profile, task.ordinal)?
                .into_iter()
                .map(|attempt| attempt.matrix)
                .collect(),
            Queue::Filler => {
                let cursor = record.filler.ok_or(NotReady::UnmatchedCell)?;
                match record.observed.tested {
                    Tested::Isolated { ccd, offset } => {
                        vec![mapping_cells(layout, ccd, offset, cursor)?]
                    }
                    Tested::Exact {} => vec![qualification_cells(
                        layout,
                        profile,
                        cursor,
                        &Reuse::AllFresh,
                    )?],
                }
            }
        };
        matrices
            .iter()
            .flat_map(|matrix| matrix.cells.iter())
            .map(|planned| planned.cell)
            .find(|cell| Sig::of_cell(cell) == sig)
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
                class: Self::exposure_cell(layout, record)?,
            });
        }
        let zero = CcdProfile::zero(self.ccds())?;
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
            rerun: self.pending_rerun(layout)?,
        })
    }

    /// The oldest nonzero-profile failure that named a core at CO 0 and has no all-zero rerun since.
    fn pending_rerun(&self, layout: &Layout<'_>) -> Result<Option<Work>, NotReady> {
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
                let cell = Self::exposure_cell(layout, record)?;
                return Ok(Some(Work::co_zero_rerun(&cell, &observed.profile.ccd)));
            }
        }
        Ok(None)
    }
}
