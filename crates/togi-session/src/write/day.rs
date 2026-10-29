//! The stock-only day write: every core to CO 0 through the night's own transaction chain.
//!
//! A day applies only the stock profile. [`apply_day_stock`] checks the requested vector against the recorded
//! topology before any command, then runs [`apply`] over a journal that only appends, so the intent, the
//! receipt-authorized write and the per-core readbacks are the ones the night's writes make.

use togi_journal::{
    CcdVector, Journal, Payload, Seq, SmuCommand, SmuIntentReceipt, SmuWritten, Stamp,
};
use togi_machine::{
    CcdProfile, CcdProfileError, Clock, CoreId, CoreInfo, CoreSet, ObservedOffset, PerCore, Smu,
    Stopped, TargetOffset,
};

use super::{Hardware, Ledger, WriteFailure, apply};
use crate::error::{AppendFailure, JournalFailure};

/// The stock profile a day wrote and read back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StockApplied {
    /// What the machine holds: every CCD at CO 0, whatever was requested.
    pub target: CcdVector,
    /// What each core read back after the write.
    pub readbacks: PerCore<ObservedOffset>,
    /// The `smu.intent`, `smu.write` and `smu.readback` events of the transaction, in journal order.
    pub transactions: Vec<Seq>,
}

/// Why a day's stock write did not complete.
#[derive(Debug, thiserror::Error)]
pub enum DayWriteError {
    /// Nothing was written: the requested CCD profile does not cover exactly the recorded topology's CCDs.
    #[error("expand the requested CCD profile over the recorded topology")]
    Profile(#[source] CcdProfileError),
    /// Nothing was written: the requested per-core offsets are not the expansion of the requested CCD profile.
    #[error("the requested per-core offsets disagree with its CCD profile on core {core:02}")]
    Expansion { core: CoreId },
    /// The transaction failed at some step; the events it journaled stay.
    #[error("write the stock day profile")]
    Write(#[from] WriteFailure),
}

/// A journal that only appends, stamped from the day's clock: a day has no run state to fold events into. The
/// journal poisons itself on a failed write, so a later append is refused as it is for the run's appender.
struct DayLedger<'a> {
    journal: &'a mut Journal,
    clock: &'a dyn Clock,
    appended: Vec<Seq>,
}

impl DayLedger<'_> {
    fn stamp(&self) -> Result<Stamp, JournalFailure> {
        Ok(Stamp {
            time: self.clock.now(),
            mono: Some(self.clock.monotonic().map_err(JournalFailure::clock)?),
        })
    }
}

impl Ledger for DayLedger<'_> {
    fn append(&mut self, payload: Payload, causes: &[Seq]) -> Result<Seq, AppendFailure> {
        let stamp = self.stamp()?;
        let seq = self
            .journal
            .append(stamp, payload, causes)
            .map_err(JournalFailure::new)?
            .seq;
        self.appended.push(seq);
        Ok(seq)
    }

    fn smu_intent(
        &mut self,
        command: SmuCommand,
        causes: &[Seq],
    ) -> Result<SmuIntentReceipt, AppendFailure> {
        let stamp = self.stamp()?;
        let receipt = self
            .journal
            .append_smu_intent(stamp, command, causes)
            .map_err(JournalFailure::new)?;
        self.appended.push(receipt.intent());
        Ok(receipt)
    }

    fn smu_written(&mut self, receipt: SmuIntentReceipt) -> Result<SmuWritten, AppendFailure> {
        let stamp = self.stamp()?;
        let written = self
            .journal
            .append_smu_write(stamp, receipt)
            .map_err(JournalFailure::new)?;
        self.appended.push(written.seq());
        Ok(written)
    }
}

/// Puts the stock profile, every core at CO 0, on the machine and reads every core back.
///
/// `requested` is what the day was asked to apply; only its shape is used. It is checked before any command:
/// its CCD profile must cover exactly the CCDs of `topology`, and its per-core offsets must be that profile's
/// expansion over `topology`, which is the machine's core set. A vector decoded from a journal keeps whatever was
/// recorded, so a malformed one is refused here. The command written is always zero, whatever the vector asks.
///
/// Success is every core reading back 0. The caller records `day.applied` with the returned target.
///
/// # Errors
/// [`DayWriteError::Profile`] and [`DayWriteError::Expansion`] refuse the request with nothing journaled. A
/// [`DayWriteError::Write`] is the transaction's own failure: a journal failure, a failed or crashed SMU command or
/// a readback that is not 0.
pub fn apply_day_stock(
    journal: &mut Journal,
    clock: &dyn Clock,
    smu: &mut dyn Smu,
    stopped: &Stopped,
    topology: &[CoreInfo],
    requested: &CcdVector,
    causes: &[Seq],
) -> Result<StockApplied, DayWriteError> {
    let expanded = requested
        .ccd()
        .expand(topology)
        .map_err(DayWriteError::Profile)?;
    let named: CoreSet = expanded
        .iter()
        .map(|(core, _)| core)
        .chain(requested.cores().iter().map(|(core, _)| core))
        .collect();
    if let Some(core) = named
        .iter()
        .find(|&core| expanded.get(core) != requested.cores().get(core))
    {
        return Err(DayWriteError::Expansion { core });
    }

    let stock = CcdProfile::zero(requested.ccd().ccds()).map_err(DayWriteError::Profile)?;
    let target = CcdVector::expand(stock, topology).map_err(DayWriteError::Profile)?;
    let mut hardware = Hardware::new(topology.iter().map(|info| info.core).collect());
    let mut ledger = DayLedger {
        journal,
        clock,
        appended: Vec::new(),
    };
    let verified = apply(&mut ledger, smu, stopped, hardware.zero_all(), causes)?;
    drop(verified);
    // `apply` succeeds only when every core read back exactly the target, which is zero.
    let readbacks = topology
        .iter()
        .map(|info| (info.core, TargetOffset::ZERO.observed()))
        .collect();
    Ok(StockApplied {
        target,
        readbacks,
        transactions: ledger.appended,
    })
}

#[cfg(test)]
mod tests;
