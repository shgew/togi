//! Operator stock attestation and historical all-zero failure safety.

use super::State;
use super::ledger::cell_of_class;
use super::night_query::NotReady;
use crate::night::Cell;

impl State {
    /// Whether the current firmware context has a durable operator attestation of stock stability.
    #[must_use]
    pub fn stock_attested(&self) -> bool {
        self.machine
            .firmware
            .as_ref()
            .is_some_and(|firmware| self.machine.stock_attestations.contains(firmware))
    }

    /// Historical all-zero failures remain safety evidence, but never request a new stock trial.
    pub(super) fn stock_failure(&self) -> Result<Option<Cell>, NotReady> {
        let Some(record) = self
            .current_groups()
            .filter_map(|group| group.zero_failure)
            .filter_map(|index| self.ledger.records.get(index))
            .min_by_key(|record| record.seq)
        else {
            return Ok(None);
        };
        let topology = self.machine.topology.as_ref().ok_or(NotReady::NoSession)?;
        cell_of_class(&record.observed.class, topology, &self.machine.cores)
            .map(Some)
            .ok_or(NotReady::UnmatchedCell)
    }
}
