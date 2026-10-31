//! Stock controls: the all-zero matrix a first night and every firmware change must pass, the CO-0 rerun a
//! nonzero failure on a zero-offset core owes, and the dead end a conclusive all-zero failure is.

use std::collections::HashSet;

use togi_journal::{Activity, Seq, SessionId, Tested};
use togi_machine::BiosContext;

use crate::UnitProfile;

use super::State;
use super::books::Exposure;
use super::ledger::{Sig, cell_of_class, is_adequate};
use super::night_query::NotReady;
use crate::night::{Cell, ControlCause, Layout, Reason, Stock, Work, control_cells};

/// The evidence context a controls scan was made under.
#[derive(Clone, Debug, PartialEq)]
struct ControlsContext {
    session: Option<SessionId>,
    since: Option<Seq>,
    firmware: Option<BiosContext>,
    changes: u32,
    contents: usize,
}

/// Where the scan for the first completion of the stock matrix stands.
#[derive(Clone, Debug, Default)]
pub(super) struct ControlsFreeze {
    context: Option<ControlsContext>,
    /// How many ledger records and R7 loads it has read.
    records: usize,
    loads: usize,
    /// The classes an adequate all-zero control passed so far.
    passed: HashSet<Sig>,
    /// The matrix completed at some position already read.
    complete: bool,
}

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
        if !self.controls_complete()? {
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
        }
        Ok(Stock::Cleared {
            rerun: self.pending_rerun()?,
        })
    }

    /// Whether the entire stock matrix has completed in the current evidence context: it completes at the
    /// first journal position where every cell of the matrix the chains known there declare has an
    /// adequate all-zero pass. A later chain change cannot reopen it, so the answer is the one the journal
    /// up to that position gives, however many queries came between. Each query reads only the exposures
    /// and R7 loads folded since the last, until the completion is found.
    fn controls_complete(&self) -> Result<bool, NotReady> {
        let mut guard = self.caches.controls.lock();
        let context = ControlsContext {
            session: self.machine.session,
            since: self.machine.context_since,
            firmware: self.machine.firmware.clone(),
            changes: self.machine.evidence_changes,
            contents: self.machine.contents.len(),
        };
        if guard.context.as_ref() != Some(&context) {
            *guard = ControlsFreeze {
                context: Some(context),
                ..ControlsFreeze::default()
            };
        }
        let freeze = &mut *guard;
        while !freeze.complete {
            while self
                .trials
                .loads
                .get(freeze.loads)
                .is_some_and(|&at| self.load_end(at).is_none())
            {
                freeze.loads += 1;
            }
            let load = self
                .trials
                .loads
                .get(freeze.loads)
                .and_then(|&at| self.load_end(at));
            let at = match (self.ledger.records.get(freeze.records), load) {
                (Some(record), Some(load)) if load < record.seq => {
                    freeze.loads += 1;
                    load
                }
                (Some(record), _) => {
                    freeze.records += 1;
                    let observed = &record.observed;
                    if observed.activity != Activity::Control
                        || !observed.profile.ccd().is_zero()
                        || observed.tested != (Tested::Exact {})
                        || !is_adequate(observed, &self.machine.cores)
                        || !self.is_current_exposure(observed)
                        || !freeze.passed.insert(Sig::of_class(&observed.class))
                    {
                        continue;
                    }
                    record.seq
                }
                (None, Some(load)) => {
                    freeze.loads += 1;
                    load
                }
                (None, None) => break,
            };
            let ordinal = self.control_ordinal()?;
            let passed = &freeze.passed;
            freeze.complete = self.layout_at(Some(at), |layout| {
                let zero = UnitProfile::zero(&crate::Units::coarse(layout.topology));
                let matrix = control_cells(layout, &zero, ordinal)?;
                Ok(matrix
                    .fresh()
                    .all(|cell| passed.contains(&Sig::of_cell(cell))))
            })?;
        }
        Ok(freeze.complete)
    }

    /// The sequence number at which a full-CCD R7 load of the current firmware ended.
    fn load_end(&self, at: usize) -> Option<Seq> {
        let trial = self.trials.all.get(at)?;
        if trial.firmware != self.machine.firmware {
            return None;
        }
        trial.end.as_ref().map(|ended| ended.seq)
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
