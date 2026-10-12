//! The one path an event takes into the journal.

use std::io::{self, Write};

use jiff::tz::TimeZone;
use togi_defect::{DefectState, Entry, KNOWN_DEFECTS};
use togi_journal::{Event, Journal, Payload, Seq, SmuCommand, SmuIntentReceipt, SmuWritten, Stamp};
use togi_machine::{CancellationToken, Clock};
use togi_render::Renderer;

use crate::boot::BootHandoff;
use crate::error::{AppendFailure, FoldError, JournalFailure};
use crate::recovery::RecoveryState;
use crate::resume::ResumeCapsule;

/// Called with each event right after it is durable, where its log line is written.
pub type AppendHook = Box<dyn FnMut(&Event)>;

/// A journal that does not fold into the run's state, handed back with the reason.
#[derive(Debug)]
pub(crate) struct Unresumed {
    pub journal: Journal,
    pub error: FoldError,
}

/// Every reducer the run keeps over its journal, each fed every event once and in order: the recovery fold first,
/// then the tuner state, then the defect state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Folds {
    pub recovery: RecoveryState,
    pub tuner: togi_tuner::State,
    pub defects: DefectState,
    pub facts: togi_journal::State,
}

impl Folds {
    pub(crate) fn replay<'a>(
        events: impl IntoIterator<Item = &'a Event>,
    ) -> Result<Self, FoldError> {
        Self::replay_with(KNOWN_DEFECTS, events)
    }

    /// Replays with the defect state judging by `defects` instead of the defects this build knows.
    pub(crate) fn replay_with<'a>(
        defects: &'static [Entry],
        events: impl IntoIterator<Item = &'a Event>,
    ) -> Result<Self, FoldError> {
        let mut folds = Self {
            defects: DefectState::new(defects),
            ..Self::default()
        };
        for event in events {
            folds.apply(event)?;
        }
        Ok(folds)
    }

    /// Folds one event. A tuner state that cannot take it in stays poisoned: the journal is corrupt.
    pub(crate) fn apply(&mut self, event: &Event) -> Result<(), FoldError> {
        self.recovery.apply(event)?;
        self.tuner.fold(event).map_err(|error| FoldError::Tuner {
            seq: event.seq,
            error,
        })?;
        self.defects.fold(event);
        self.facts.apply(event)?;
        Ok(())
    }
}

/// Stamps an event from the machine clock, appends it, then folds it into the run's reducers, writes its log line
/// and, after the first durable append, runs the tuning-boot handoff.
///
/// The first failed append latches: every later append returns that failure without touching the journal, and the
/// running trial's token is cancelled so its workload stops.
pub(crate) struct Appender {
    pub(crate) journal: Journal,
    clock: Box<dyn Clock>,
    pub(crate) folds: Folds,
    renderer: Renderer,
    zone: TimeZone,
    log: Option<Box<dyn Write>>,
    log_failure: Option<io::Error>,
    on_append: Option<AppendHook>,
    latched: Option<JournalFailure>,
    trial: Option<CancellationToken>,
    handoff: Option<BootHandoff>,
    defects: &'static [Entry],
    pub(crate) publication: Option<crate::publication::PublicationPolicy>,
    pub(crate) publishing: bool,
    /// The folds came from a capsule that covers the journal: the run before it left `state.json` pending on purpose,
    /// so there is nothing to rebuild.
    pub(crate) adopted: bool,
}

impl Appender {
    /// Takes over an opened journal, folding the events it already holds into the run's state without logging them.
    #[cfg(test)]
    pub(crate) fn resume(
        journal: Journal,
        clock: Box<dyn Clock>,
        renderer: Renderer,
        zone: TimeZone,
    ) -> Result<Self, FoldError> {
        Self::resume_with(journal, clock, renderer, zone, KNOWN_DEFECTS, None)
            .map_err(|unresumed| unresumed.error)
    }

    /// [`Appender::resume`] with the defects to judge by and the capsule of an earlier run, whose folds are taken
    /// over when it covers exactly this journal. A journal that does not fold is handed back.
    pub(crate) fn resume_with(
        journal: Journal,
        clock: Box<dyn Clock>,
        renderer: Renderer,
        zone: TimeZone,
        defects: &'static [Entry],
        capsule: Option<ResumeCapsule>,
    ) -> Result<Self, Box<Unresumed>> {
        let adopted = capsule.and_then(|capsule| capsule.adopt(&journal, defects));
        let was_adopted = adopted.is_some();
        let folds = match adopted {
            Some(folds) => folds,
            None => match Folds::replay_with(defects, journal.events()) {
                Ok(folds) => folds,
                Err(error) => return Err(Box::new(Unresumed { journal, error })),
            },
        };
        Ok(Self {
            journal,
            clock,
            folds,
            renderer,
            zone,
            log: None,
            log_failure: None,
            on_append: None,
            latched: None,
            trial: None,
            handoff: None,
            defects,
            publication: None,
            publishing: false,
            adopted: was_adopted,
        })
    }

    /// The capsule that lets the next run skip folding this appender's journal.
    pub(crate) fn capsule(&self) -> ResumeCapsule {
        ResumeCapsule::new(&self.journal, self.defects, self.folds.clone())
    }

    #[must_use]
    pub(crate) fn with_log(mut self, log: Box<dyn Write>) -> Self {
        self.log = Some(log);
        self
    }

    #[must_use]
    pub(crate) fn with_on_append(mut self, hook: AppendHook) -> Self {
        self.on_append = Some(hook);
        self
    }

    #[must_use]
    pub(crate) fn with_handoff(mut self, handoff: BootHandoff) -> Self {
        self.handoff = Some(handoff);
        self
    }

    pub(crate) const fn journal(&self) -> &Journal {
        &self.journal
    }

    pub(crate) const fn recovery(&self) -> &RecoveryState {
        &self.folds.recovery
    }

    pub(crate) const fn tuner(&self) -> &togi_tuner::State {
        &self.folds.tuner
    }

    pub(crate) const fn defects(&self) -> &DefectState {
        &self.folds.defects
    }

    pub(crate) fn clock(&mut self) -> &mut dyn Clock {
        &mut *self.clock
    }

    pub(crate) fn handoff(&mut self) -> Option<&mut BootHandoff> {
        self.handoff.as_mut()
    }

    /// The failure that latched the appender, if any append has failed.
    pub(crate) const fn failure(&self) -> Option<&JournalFailure> {
        self.latched.as_ref()
    }

    /// Takes the first error writing the event log. The log is dropped after it, and the run goes on.
    pub(crate) const fn take_log_failure(&mut self) -> Option<io::Error> {
        self.log_failure.take()
    }

    /// The token of the trial that is running, cancelled when an append fails; `None` once it ended.
    pub(crate) fn watch_trial(&mut self, token: Option<CancellationToken>) {
        self.trial = token;
    }

    pub(crate) fn into_journal(self) -> Journal {
        self.journal
    }
    /// Acknowledges the tuning-boot leave reason still pending, as after a resumed dead end.
    pub(crate) fn import_boot_reason(&mut self) -> Result<(), AppendFailure> {
        let Some(mut handoff) = self.handoff.take() else {
            return Ok(());
        };
        let result = handoff.import(self);
        self.handoff = Some(handoff);
        result
    }

    pub(crate) fn append(
        &mut self,
        payload: Payload,
        causes: &[Seq],
    ) -> Result<Seq, AppendFailure> {
        let stamp = self.stamp()?;
        let seq = match self.journal.append(stamp, payload, causes) {
            Ok(event) => event.seq,
            Err(error) => return Err(self.latch(JournalFailure::new(error)).into()),
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
            Err(error) => return Err(self.latch(JournalFailure::new(error)).into()),
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
            Err(error) => return Err(self.latch(JournalFailure::new(error)).into()),
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

    fn latch(&mut self, failure: JournalFailure) -> JournalFailure {
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
            if let Some(hook) = self.on_append.as_mut() {
                hook(event);
            }
            if let Err(error) = self.folds.apply(event) {
                self.latch(JournalFailure::fold(error.clone()));
                return Err(error.into());
            }
        }
        if let Some(mut handoff) = self.handoff.take() {
            let result = handoff.after_durable_append(self);
            self.handoff = Some(handoff);
            result?;
        }
        if self.publication == Some(crate::publication::PublicationPolicy::Live) && !self.publishing
        {
            self.publish_state()?;
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

#[cfg(test)]
mod tests;
