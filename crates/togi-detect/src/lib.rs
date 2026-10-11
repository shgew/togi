mod boot;
mod error;
mod journal;
mod parse;
mod pstore;
mod reset;
mod runner;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Duration;

use togi_machine::{
    BootId, CoreInfo, KernelCursor, KernelRead, Mce, Partial, PstoreRecord, ResetReason,
};

pub use boot::boot_id;
pub use error::{BootIdError, Error, Operation};
pub use journal::DecodeError;
pub use parse::{CpuCoreMap, DuplicateCpu, Message, ParseFailure, StatusError, parse};
pub use pstore::{Access, PstoreError, Unsafe};
pub use reset::VersionError;

use boot::JournalBoot;
use journal::{
    Edge, ExitRules, IntervalError, boot_entries, boots_args, boundary_args, interval_args,
    mce_messages, mces_args, position, read_interval, reset_args, reset_texts,
};
use pstore::{Archive, BootSpan, PSTORE_DIR};
use reset::ResetScan;
use runner::{JOURNALCTL_BUDGET, Journal, Journalctl, Process};

/// Kernel-log evidence of a machine's boots, read through `journalctl` and the pstore archive.
pub struct Kernel {
    cores: CpuCoreMap,
    journalctl: Box<dyn Journalctl>,
    budget: Duration,
    pstore: PathBuf,
}

impl Kernel {
    pub fn new(topology: &[CoreInfo]) -> Result<Self, DuplicateCpu> {
        Self::with_parts(
            topology,
            Box::new(Process::system()),
            JOURNALCTL_BUDGET,
            PathBuf::from(PSTORE_DIR),
        )
    }

    fn with_parts(
        topology: &[CoreInfo],
        journalctl: Box<dyn Journalctl>,
        budget: Duration,
        pstore: PathBuf,
    ) -> Result<Self, DuplicateCpu> {
        Ok(Self {
            cores: CpuCoreMap::new(topology)?,
            journalctl,
            budget,
            pstore,
        })
    }

    fn journal(&self) -> Journal<'_> {
        Journal::new(self.journalctl.as_ref(), self.budget)
    }

    fn decode_failure(operation: &Operation, source: DecodeError) -> Error {
        Error::Decode {
            operation: operation.clone(),
            source,
        }
    }

    fn read_whole_boot(&self, boot: &JournalBoot, since: Duration) -> Result<Vec<Mce>, Error> {
        let operation = Operation::ReadKernelLog {
            boot: boot.as_str().to_owned(),
        };
        let out = self
            .journal()
            .query(&operation, &mces_args(boot), ExitRules::GREP)?;
        let messages =
            mce_messages(&out, since).map_err(|source| Self::decode_failure(&operation, source))?;
        parse(&messages, &self.cores).map_err(|failure| Error::Status {
            operation,
            source: failure.error,
        })
    }

    fn read_interval(
        &self,
        boot: &JournalBoot,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<Mce>>> {
        let operation = Operation::ReadKernelLog {
            boot: boot.as_str().to_owned(),
        };
        let after = after.filter(|cursor| !cursor.as_str().is_empty());
        let nothing = |error: Error| Partial {
            value: Vec::new(),
            error: error.into(),
        };
        let out = self
            .journal()
            .query(
                &operation,
                &interval_args(boot, after),
                ExitRules::cursor_read(after.is_some()),
            )
            .map_err(nothing)?;
        let interval = read_interval(&out, boot, after);
        let (mces, parse_error) = match parse(&interval.messages, &self.cores) {
            Ok(mces) => (mces, None),
            Err(failure) => (failure.mces, Some(failure.error)),
        };
        let error = match (parse_error, interval.end) {
            (Some(source), _) => Error::Status { operation, source },
            (None, Ok(cursor)) => return Ok(KernelRead { mces, cursor }),
            (None, Err(IntervalError::CursorMissing)) => Error::CursorMissing,
            (None, Err(IntervalError::Decode(source))) => Self::decode_failure(&operation, source),
        };
        Err(Partial {
            value: mces,
            error: error.into(),
        })
    }

    fn read_reset_reason(journal: &Journal<'_>, boot: &JournalBoot) -> Result<ResetReason, Error> {
        let operation = Operation::ReadResetReason {
            boot: boot.as_str().to_owned(),
        };
        let out = journal.query(&operation, &reset_args(boot), ExitRules::GREP)?;
        let mut scan = ResetScan::default();
        for text in reset_texts(&out).map_err(|source| Self::decode_failure(&operation, source))? {
            scan.push(&text).map_err(|source| Error::Version {
                operation: operation.clone(),
                source,
            })?;
        }
        Ok(scan.finish())
    }

    fn read_reset_reason_after(&self, boot: &JournalBoot) -> Result<ResetReason, Error> {
        let journal = self.journal();
        let operation = Operation::ListBoots;
        let out = journal.query(
            &operation,
            &boots_args(),
            ExitRules::BOOT_MISSING_WHEN_EMPTY,
        )?;
        let boots =
            boot_entries(&out).map_err(|source| Self::decode_failure(&operation, source.into()))?;
        let successor = boots
            .iter()
            .position(|entry| entry.boot_id == boot.as_str())
            .and_then(|at| boots.get(at + 1))
            .filter(|next| !next.boot_id.is_empty())
            .ok_or(Error::BootMissing)?;
        let successor = JournalBoot::parse(&successor.boot_id)?;
        let last = Self::boundary(&journal, boot, Edge::Last)?;
        let first = Self::boundary(&journal, &successor, Edge::First)?;
        if !last.is_followed_by(&first) {
            return Err(Error::BootMissing);
        }
        Self::read_reset_reason(&journal, &successor)
    }

    fn boundary(
        journal: &Journal<'_>,
        boot: &JournalBoot,
        edge: Edge,
    ) -> Result<reset::Position, Error> {
        let operation = Operation::ReadJournalBoundary {
            boot: boot.as_str().to_owned(),
        };
        let out = journal.query(
            &operation,
            &boundary_args(boot, edge),
            ExitRules::BOOT_MISSING_WHEN_EMPTY,
        )?;
        position(&out, boot)
            .map_err(|source| Self::decode_failure(&operation, source))?
            .ok_or(Error::BootMissing)
    }

    fn read_saved_pstore(&self, boot: &JournalBoot) -> Result<Option<PstoreRecord>, Error> {
        let Some(archive) = Archive::open(&self.pstore)? else {
            return Ok(None);
        };
        let operation = Operation::ListBoots;
        let out = self
            .journal()
            .query(&operation, &boots_args(), ExitRules::PLAIN)?;
        let boots =
            boot_entries(&out).map_err(|source| Self::decode_failure(&operation, source.into()))?;
        let Some(span) = BootSpan::of(&boots, boot) else {
            return Ok(None);
        };
        let Some(dump) = archive.newest_dump(span)? else {
            return Ok(None);
        };
        Ok(Some(PstoreRecord {
            path: dump.path().to_string_lossy().into_owned(),
            lines: dump.tail()?,
        }))
    }
}

impl togi_machine::Kernel for Kernel {
    fn mces(&mut self, boot: &BootId, since: Duration) -> Result<Vec<Mce>, togi_machine::Error> {
        let boot = JournalBoot::parse(boot.as_str())?;
        Ok(self.read_whole_boot(&boot, since)?)
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<Mce>>> {
        let boot = JournalBoot::parse(boot.as_str()).map_err(|error| Partial {
            value: Vec::new(),
            error: error.into(),
        })?;
        self.read_interval(&boot, after)
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, togi_machine::Error> {
        let boot = JournalBoot::parse(boot.as_str())?;
        Ok(Self::read_reset_reason(&self.journal(), &boot)?)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, togi_machine::Error> {
        let boot = JournalBoot::parse(boot.as_str())?;
        Ok(self.read_reset_reason_after(&boot)?)
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, togi_machine::Error> {
        let boot = JournalBoot::parse(boot.as_str())?;
        Ok(self.read_saved_pstore(&boot)?)
    }
}
