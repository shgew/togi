//! The day's profile write: the requested profile, validated, through the night's own transaction chain.
//!
//! [`apply_day_profile`] checks the requested vector against the recorded topology before anything is journaled,
//! then runs [`apply`] over a journal that only appends: every core to 0, then each nonzero core in core order, then
//! a read of every core. The intent, the receipt-authorized write and the readbacks are the ones the night's writes
//! make. A failure restores every core to 0.

use togi_journal::{
    CcdVector, Journal, Payload, Seq, SmuCommand, SmuIntentReceipt, SmuOp, SmuReadback, SmuWritten,
    Stamp,
};
use togi_machine::{
    CcdProfileError, Clock, CoreId, CoreInfo, CoreSet, ObservedOffset, PerCore, Smu, SmuValidated,
    Stopped, TargetOffset,
};

use super::{Hardware, Ledger, ProfileFailure, WriteFailure, apply, failed};
use crate::emergency::{Zeroed, zero_and_read_back};
use crate::error::{AppendFailure, JournalFailure};

/// The profile a day wrote and read back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayAppliedProfile {
    /// What the machine holds: exactly the requested vector.
    pub target: CcdVector,
    /// What each core read back after the write.
    pub readbacks: PerCore<ObservedOffset>,
    /// The `smu.intent`, `smu.write` and `smu.readback` events of the transaction, in journal order.
    pub transactions: Vec<Seq>,
}

/// Why a day's profile write did not complete.
#[derive(Debug, thiserror::Error)]
pub enum DayWriteError {
    /// Nothing was written or journaled: the requested CCD profile does not cover exactly the recorded topology's
    /// CCDs.
    #[error("expand the requested CCD profile over the recorded topology")]
    Profile(#[source] CcdProfileError),
    /// Nothing was written or journaled: the requested per-core offsets are not the expansion of the requested CCD
    /// profile.
    #[error("the requested per-core offsets disagree with its CCD profile on core {core:02}")]
    Expansion { core: CoreId },
    /// The transaction failed at some step. The events it journaled stay; `recovery` says what became of the
    /// offsets already written.
    #[error("write the day profile")]
    Write {
        #[source]
        failure: ProfileFailure,
        recovery: Box<DayRecovery>,
    },
}

/// What a failed day write did to put every core back at 0.
#[derive(Debug)]
pub enum DayRecovery {
    /// The machine crashed: nothing touched the SMU afterwards.
    Crashed,
    /// The journal failed, so nothing more can be appended: every core was zeroed without an intent and read back,
    /// which `zeroed` reports. The journal records neither the zeroing nor its readbacks.
    Emergency(Zeroed),
    /// Every core was zeroed through the ordinary intent, write and readback chain, and each read back 0.
    Restored,
    /// The restoring transaction failed at an SMU step or a readback; the offsets are unconfirmed and nothing else
    /// was tried. The failure is journaled where the journal allowed.
    Unrestored(WriteFailure),
    /// The journal failed during the restoring transaction: `zeroed` reports the zeroing without an intent that
    /// followed it.
    RestorePoisoned {
        journal: JournalFailure,
        zeroed: Zeroed,
    },
}

/// A journal that only appends, stamped from the day's clock: a day has no run state to fold events into. The
/// journal poisons itself on a failed write, so a later append is refused as it is for the run's appender.
struct DayLedger<'a> {
    journal: &'a mut Journal,
    clock: &'a dyn Clock,
    appended: Vec<Seq>,
    /// The latest `smu.write`, which the final all-core readbacks cite.
    last_write: Option<Seq>,
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
        self.last_write = Some(written.seq());
        Ok(written)
    }
}

/// Puts the requested profile on the machine and reads every core back.
///
/// `requested` is checked before anything is journaled: its CCD profile must cover exactly the CCDs of `topology`,
/// and its per-core offsets must be that profile's expansion over `topology`, which is the machine's core set. A
/// vector decoded from a journal keeps whatever was recorded, so a malformed one is refused here. The tuner decides
/// whether the profile is permitted; this writer applies the exact vector it is given.
///
/// Every core is first written to 0 (the machine's profile is not known), then each core whose target is not 0 gets
/// its own durable intent, write and readback, in core order, and finally every core is read back against its
/// target. Success is every core reading back exactly its target, and the caller records `day.applied` with the
/// returned vector.
///
/// A failure after the first intent leaves offsets that may be nonzero, so it restores every core to 0: through the
/// ordinary transaction while the journal accepts appends, or with the zero-only emergency, which needs `validated`,
/// when the journal failed. A crash ends all SMU access at once.
///
/// # Errors
/// [`DayWriteError::Profile`] and [`DayWriteError::Expansion`] refuse the request with nothing journaled and no
/// hardware access. [`DayWriteError::Write`] carries the transaction's own failure (a journal failure, a failed or
/// crashed SMU command or a readback that is not the target) and the [`DayRecovery`] that followed it.
#[expect(
    clippy::too_many_arguments,
    reason = "the day's journal, clock, SMU and its two proofs, the recorded topology and the request are each distinct borrows with no owner to group them"
)]
pub fn apply_day_profile(
    journal: &mut Journal,
    clock: &dyn Clock,
    smu: &mut dyn Smu,
    validated: SmuValidated,
    stopped: &Stopped,
    topology: &[CoreInfo],
    requested: &CcdVector,
    causes: &[Seq],
) -> Result<DayAppliedProfile, DayWriteError> {
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

    let cores: CoreSet = topology.iter().map(|info| info.core).collect();
    let mut hardware = Hardware::new(cores);
    let mut ledger = DayLedger {
        journal,
        clock,
        appended: Vec::new(),
        last_write: None,
    };
    match transact(&mut ledger, smu, stopped, &mut hardware, &expanded, causes) {
        Ok(()) => Ok(DayAppliedProfile {
            target: requested.clone(),
            // `transact` succeeds only when every core read back exactly its target.
            readbacks: expanded
                .iter()
                .map(|(core, target)| (core, target.observed()))
                .collect(),
            transactions: ledger.appended,
        }),
        Err(failure) => {
            let recovery = recover(
                &failure,
                &mut ledger,
                smu,
                validated,
                stopped,
                &mut hardware,
                cores,
            );
            Err(DayWriteError::Write {
                failure,
                recovery: Box::new(recovery),
            })
        }
    }
}

/// Zero, each nonzero core, then every core read against its target. After the zero every core has been read back at
/// 0, so a request of stock needs no further pass.
fn transact(
    ledger: &mut DayLedger<'_>,
    smu: &mut dyn Smu,
    stopped: &Stopped,
    hardware: &mut Hardware,
    expanded: &PerCore<TargetOffset>,
    causes: &[Seq],
) -> Result<(), ProfileFailure> {
    drop(apply(ledger, smu, stopped, hardware.zero_all(), causes)?);
    let mut wrote = false;
    for (core, wanted) in expanded.iter() {
        if wanted == TargetOffset::ZERO {
            continue;
        }
        let checked = hardware.check_set(core, wanted)?;
        drop(apply(ledger, smu, stopped, checked, causes)?);
        wrote = true;
    }
    if wrote {
        confirm(ledger, smu, expanded)?;
    }
    Ok(())
}

/// Reads every core and records each against the target it must hold, citing the latest write. The first core that
/// reads something else ends the pass; its readback is durable.
fn confirm(
    ledger: &mut DayLedger<'_>,
    smu: &mut dyn Smu,
    expanded: &PerCore<TargetOffset>,
) -> Result<(), WriteFailure> {
    let cause: Vec<Seq> = ledger.last_write.into_iter().collect();
    for (core, target) in expanded.iter() {
        let observed = match smu.offset(core) {
            Ok(observed) => observed,
            Err(error) => {
                return Err(failed(
                    ledger,
                    SmuOp::Read,
                    Some(core),
                    Some(target),
                    &cause,
                    error,
                ));
            }
        };
        ledger.append(
            Payload::SmuReadback(SmuReadback {
                core,
                offset: observed,
                expected: Some(target),
            }),
            &cause,
        )?;
        if observed != target.observed() {
            return Err(WriteFailure::Mismatch {
                core,
                observed,
                expected: target,
            });
        }
    }
    Ok(())
}

/// Puts every core back at 0 after `failure`. A crash allows no SMU access. A failed journal allows only the
/// emergency zeroing; any other failure is answered with the ordinary zeroing transaction, which may in turn find
/// the journal failed.
fn recover(
    failure: &ProfileFailure,
    ledger: &mut DayLedger<'_>,
    smu: &mut dyn Smu,
    validated: SmuValidated,
    stopped: &Stopped,
    hardware: &mut Hardware,
    cores: CoreSet,
) -> DayRecovery {
    match failure {
        ProfileFailure::Write(WriteFailure::Crashed) => DayRecovery::Crashed,
        ProfileFailure::Write(WriteFailure::Journal(_)) => {
            DayRecovery::Emergency(zero_and_read_back(smu, validated, stopped, cores))
        }
        ProfileFailure::Refused(_)
        | ProfileFailure::Shape { .. }
        | ProfileFailure::Write(
            WriteFailure::Record(_) | WriteFailure::Smu { .. } | WriteFailure::Mismatch { .. },
        ) => {
            let causes: Vec<Seq> = ledger.appended.last().copied().into_iter().collect();
            match apply(ledger, smu, stopped, hardware.zero_all(), &causes) {
                Ok(verified) => {
                    drop(verified);
                    DayRecovery::Restored
                }
                Err(WriteFailure::Crashed) => DayRecovery::Crashed,
                Err(WriteFailure::Journal(journal)) => DayRecovery::RestorePoisoned {
                    journal,
                    zeroed: zero_and_read_back(smu, validated, stopped, cores),
                },
                Err(other) => DayRecovery::Unrestored(other),
            }
        }
    }
}

#[cfg(test)]
mod tests;
