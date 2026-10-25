//! The session's own records before its first trial: the BIOS context, the all-zero baseline the machine was found
//! at and the day records a fresh transition imports.

use std::collections::HashMap;

use togi_journal::{Payload, Provenance, Seq, SessionBaseline, SessionContext};
use togi_machine::CoreSet;

use crate::error::{SessionError, Step};
use crate::session::{Control, Outcome, Session};
use crate::write;

impl Session<'_> {
    /// Records what the session starts from, each piece once however often the run begins: the BIOS context (again
    /// when a resumed run finds a different one), the verified all-zero baseline and the import. A baseline that
    /// cannot be read is SMU evidence; one that is nonzero or incomplete is refused, never clamped.
    pub(crate) fn begin(&mut self) -> Outcome {
        let step = self.record_start().map(|()| Control::Continue);
        self.settle(step)
    }

    fn record_start(&mut self) -> Step<()> {
        self.record_context()?;
        self.record_baseline()?;
        self.record_import()
    }

    /// Records the BIOS context unless the journal's latest `session.context` is the machine's current one: a
    /// resumed run on changed firmware records the new context and goes on.
    fn record_context(&mut self) -> Step<()> {
        let context = self.host.bios_context().map_err(SessionError::from)?;
        if self
            .appender
            .recovery()
            .context()
            .is_some_and(|recorded| recorded.first_difference(&context).is_none())
        {
            return Ok(());
        }
        let payload = SessionContext {
            bios_version: context.bios_version,
            board: context.board,
            cpu_model: context.cpu_model,
            microcode: context.microcode,
            boost_limit_mhz: context.boost_limit_mhz,
        };
        self.appender
            .append(Payload::SessionContext(payload), &[])?;
        Ok(())
    }

    fn record_baseline(&mut self) -> Step<()> {
        if self.appender.recovery().baseline().is_some() {
            return Ok(());
        }
        let reading = write::read_all(&mut self.appender, &mut *self.smu, &mut self.hardware, &[])?;
        let missing: CoreSet = self
            .topology
            .iter()
            .map(|info| info.core)
            .filter(|core| reading.offsets.get(*core).is_none())
            .collect();
        if !missing.is_empty() {
            return Err(SessionError::BaselineIncomplete { cores: missing }.into());
        }
        let nonzero: CoreSet = reading
            .offsets
            .iter()
            .filter(|(_, offset)| offset.0 != 0)
            .map(|(core, _)| core)
            .collect();
        if !nonzero.is_empty() {
            return Err(SessionError::BaselineNotZero { cores: nonzero }.into());
        }
        let payload = SessionBaseline {
            offsets: reading.offsets,
        };
        self.appender
            .append(Payload::SessionBaseline(payload), &reading.readbacks)?;
        Ok(())
    }

    /// Appends the `day.imported` events the journal does not hold yet, in manifest order, then the
    /// `session.imported` that commits them. A journal that already holds the commitment is never imported into
    /// again; one killed before it resumes the same manifest and appends only the identities it lacks.
    fn record_import(&mut self) -> Step<()> {
        let Some(import) = self.import.as_ref() else {
            return Ok(());
        };
        let events = self.appender.journal().events();
        if events
            .iter()
            .any(|event| matches!(event.payload, Payload::SessionImported(_)))
        {
            return Ok(());
        }
        let mut held: HashMap<Provenance, Seq> = events
            .iter()
            .filter_map(|event| {
                if let Payload::DayImported(record) = &event.payload {
                    Some((record.original, event.seq))
                } else {
                    None
                }
            })
            .collect();
        let manifest = import.manifest();
        let mut causes = Vec::with_capacity(manifest.records().len());
        for record in manifest.records() {
            let seq = if let Some(seq) = held.get(&record.original) {
                *seq
            } else {
                let seq = self
                    .appender
                    .append(Payload::DayImported(record.clone()), &[])?;
                held.insert(record.original, seq);
                seq
            };
            causes.push(seq);
        }
        self.appender
            .append(Payload::SessionImported(manifest.commitment()), &causes)?;
        Ok(())
    }
}
