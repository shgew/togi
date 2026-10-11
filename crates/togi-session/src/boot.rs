//! The tuning-boot handoff: why the previous tuning boot left, and the restart count that guards against a loop.
//!
//! The pending reason is read before any append. After the first durable append of the run, whatever it is, the
//! restart count is reset once and the reason is acknowledged: appended as `boot.leave_reason` unless a live or
//! archived journal already holds its id, and cleared from the bootloader only after that event is durable. A
//! bootloader failure is an ordinary error, never a journal failure; the reason stays pending for the next run.

use std::io;

use togi_journal::{BootLeaveReason, BootSavedEntry, LookupError, Payload};
use togi_tuningboot::{self as tuningboot, Bootloader, LeaveReason, RestartCount};

use crate::appender::Appender;
use crate::error::AppendFailure;

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error(transparent)]
    Bootloader(#[from] tuningboot::Error),
    #[error("look up the journaled tuning-boot reason: {0}")]
    Lookup(#[from] LookupError),
}

fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// What leaving the tuning boot for a dead end did.
#[derive(Debug)]
pub struct Left {
    pub entry: io::Result<tuningboot::SavedEntry>,
    /// Saving the leave reason failed. It never changes whether the saved entry was cleared.
    pub reason_error: Option<BootError>,
}

impl Left {
    /// The `boot.saved_entry` event that records it.
    pub fn payload(&self) -> BootSavedEntry {
        let (before, after, error) = match &self.entry {
            Ok(entry) => (Some(entry.before.clone()), Some(entry.after.clone()), None),
            Err(error) => (None, None, Some(describe(error))),
        };
        BootSavedEntry {
            before,
            after,
            error,
            reason_error: self.reason_error.as_ref().map(|error| describe(error)),
        }
    }
}

pub struct BootHandoff {
    bootloader: Box<dyn Bootloader>,
    pending: Option<LeaveReason>,
    progressed: bool,
}

impl BootHandoff {
    /// Reads the pending leave reason. Nothing may have been appended yet.
    pub fn read(mut bootloader: Box<dyn Bootloader>) -> Result<Self, BootError> {
        let pending = tuningboot::read_reason(&mut *bootloader)?;
        Ok(Self {
            bootloader,
            pending,
            progressed: false,
        })
    }

    pub const fn pending(&self) -> Option<&LeaveReason> {
        self.pending.as_ref()
    }

    pub fn bootloader(&mut self) -> &mut dyn Bootloader {
        &mut *self.bootloader
    }

    pub(crate) fn after_durable_append(
        &mut self,
        appender: &mut Appender,
    ) -> Result<(), AppendFailure> {
        if self.progressed {
            return Ok(());
        }
        self.progressed = true;
        tuningboot::reset_count(&mut *self.bootloader).map_err(BootError::from)?;
        self.import(appender)
    }

    /// Acknowledges the pending reason in the journal, then clears it from the bootloader.
    pub(crate) fn import(&mut self, appender: &mut Appender) -> Result<(), AppendFailure> {
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        let recorded = appender
            .journal()
            .boot_reason_recorded(pending.id.as_str())
            .map_err(BootError::from)?;
        if !recorded {
            let leave = BootLeaveReason {
                reason_id: pending.id.to_string(),
                restart_limit_count: pending.count.get(),
                reason: pending.reason.to_string(),
            };
            appender.append(Payload::BootLeaveReason(leave), &[])?;
        }
        tuningboot::clear_reason(&mut *self.bootloader).map_err(BootError::from)?;
        self.pending = None;
        Ok(())
    }

    /// Leaves the tuning boot for a dead end: saves why, then clears the saved boot entry.
    pub fn leave(&mut self, reason: &str) -> Left {
        let reason_error = LeaveReason::new(reason, RestartCount::ZERO)
            .and_then(|record| tuningboot::write_reason(&mut *self.bootloader, &record))
            .err()
            .map(BootError::from);
        Left {
            entry: self.bootloader.clear_saved_entry(),
            reason_error,
        }
    }
}

#[cfg(test)]
mod tests;
