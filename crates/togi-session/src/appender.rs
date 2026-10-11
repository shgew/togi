//! The one path an event takes into the journal.

use std::io::{self, Write};

use jiff::tz::TimeZone;
use togi_journal::{
    AppendError, Event, Journal, Payload, Seq, SmuCommand, SmuIntentReceipt, SmuWritten, Stamp,
};
use togi_machine::{CancellationToken, Clock};
use togi_render::Renderer;

use crate::boot::BootHandoff;
use crate::error::{AppendFailure, FoldError, JournalFailure};
use crate::recovery::RecoveryState;

/// Stamps an event from the machine clock, appends it, then folds it into the run's reducers, writes its log line
/// and, after the first durable append, runs the tuning-boot handoff.
///
/// The first failed append latches: every later append returns that failure without touching the journal, and the
/// running trial's token is cancelled so its workload stops.
pub struct Appender {
    journal: Journal,
    clock: Box<dyn Clock>,
    recovery: RecoveryState,
    renderer: Renderer,
    zone: TimeZone,
    log: Option<Box<dyn Write>>,
    log_failure: Option<io::Error>,
    latched: Option<JournalFailure>,
    trial: Option<CancellationToken>,
    handoff: Option<BootHandoff>,
}

impl Appender {
    /// Takes over an opened journal, folding the events it already holds into the run's state without logging them.
    pub fn resume(
        journal: Journal,
        clock: Box<dyn Clock>,
        renderer: Renderer,
        zone: TimeZone,
    ) -> Result<Self, FoldError> {
        let recovery = RecoveryState::replay(journal.events())?;
        Ok(Self {
            journal,
            clock,
            recovery,
            renderer,
            zone,
            log: None,
            log_failure: None,
            latched: None,
            trial: None,
            handoff: None,
        })
    }

    #[must_use]
    pub fn with_log(mut self, log: Box<dyn Write>) -> Self {
        self.log = Some(log);
        self
    }

    #[must_use]
    pub fn with_handoff(mut self, handoff: BootHandoff) -> Self {
        self.handoff = Some(handoff);
        self
    }

    pub const fn journal(&self) -> &Journal {
        &self.journal
    }

    pub const fn recovery(&self) -> &RecoveryState {
        &self.recovery
    }

    pub fn clock(&mut self) -> &mut dyn Clock {
        &mut *self.clock
    }

    pub fn handoff(&mut self) -> Option<&mut BootHandoff> {
        self.handoff.as_mut()
    }

    /// The failure that latched the appender, if any append has failed.
    pub const fn failure(&self) -> Option<&JournalFailure> {
        self.latched.as_ref()
    }

    /// The first error writing the event log. The log is dropped after it, and the run goes on.
    pub const fn log_failure(&self) -> Option<&io::Error> {
        self.log_failure.as_ref()
    }

    /// The token of the trial that is running, cancelled when an append fails; `None` once it ended.
    pub fn watch_trial(&mut self, token: Option<CancellationToken>) {
        self.trial = token;
    }

    pub fn into_journal(self) -> Journal {
        self.journal
    }
    /// Acknowledges the tuning-boot leave reason still pending, as after a resumed dead end.
    pub fn import_boot_reason(&mut self) -> Result<(), AppendFailure> {
        let Some(mut handoff) = self.handoff.take() else {
            return Ok(());
        };
        let result = handoff.import(self);
        self.handoff = Some(handoff);
        result
    }

    pub fn append(&mut self, payload: Payload, causes: &[Seq]) -> Result<Seq, AppendFailure> {
        let stamp = self.stamp()?;
        let seq = match self.journal.append(stamp, payload, causes) {
            Ok(event) => event.seq,
            Err(error) => return Err(self.latch(error).into()),
        };
        self.absorb(seq)?;
        Ok(seq)
    }

    pub(crate) fn smu_intent(
        &mut self,
        command: SmuCommand,
        causes: &[Seq],
    ) -> Result<SmuIntentReceipt, AppendFailure> {
        let stamp = self.stamp()?;
        let receipt = match self.journal.append_smu_intent(stamp, command, causes) {
            Ok(receipt) => receipt,
            Err(error) => return Err(self.latch(error).into()),
        };
        self.absorb(receipt.intent())?;
        Ok(receipt)
    }

    pub(crate) fn smu_written(
        &mut self,
        receipt: SmuIntentReceipt,
    ) -> Result<SmuWritten, AppendFailure> {
        let stamp = self.stamp()?;
        let written = match self.journal.append_smu_write(stamp, receipt) {
            Ok(written) => written,
            Err(error) => return Err(self.latch(error).into()),
        };
        self.absorb(written.seq())?;
        Ok(written)
    }

    fn stamp(&self) -> Result<Stamp, JournalFailure> {
        match &self.latched {
            Some(failure) => Err(failure.clone()),
            None => Ok(Stamp {
                time: self.clock.now(),
                mono: Some(self.clock.monotonic()),
            }),
        }
    }

    fn latch(&mut self, error: AppendError) -> JournalFailure {
        let failure = JournalFailure::new(error);
        if self.latched.is_none() {
            self.latched = Some(failure.clone());
        }
        if let Some(token) = &self.trial {
            token.cancel();
        }
        failure
    }

    fn absorb(&mut self, seq: Seq) -> Result<(), AppendFailure> {
        if let Some(event) = self.journal.event(seq) {
            log_line(
                self.renderer,
                &self.zone,
                &mut self.log,
                &mut self.log_failure,
                event,
            );
            self.recovery.apply(event)?;
        }
        if let Some(mut handoff) = self.handoff.take() {
            let result = handoff.after_durable_append(self);
            self.handoff = Some(handoff);
            result?;
        }
        Ok(())
    }
}

fn log_line(
    renderer: Renderer,
    zone: &TimeZone,
    log: &mut Option<Box<dyn Write>>,
    failure: &mut Option<io::Error>,
    event: &Event,
) {
    let Some(writer) = log.as_mut() else {
        return;
    };
    if let Err(error) = renderer.write_event(writer, event, zone) {
        failure.get_or_insert(error);
        *log = None;
    }
}
