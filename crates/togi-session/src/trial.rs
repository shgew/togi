//! Starting a trial and journaling what it reports.

use togi_journal::{Payload, Seq, TrialId, TrialProgress, TrialSample};
use togi_machine::{
    CancellationToken, CoreId, Partial, Reporter, Running, Sample, Signal, Stopped, TrialSpec,
    Trials,
};

use crate::appender::Appender;
use crate::error::AppendFailure;
use crate::write::Verified;

/// The right to start a trial on the profile a [`Verified`] write transaction confirmed. The trial's records cite
/// the readbacks that confirmed it. Only [`authorize`] makes one, and only from a [`Verified`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Authorized<'v> {
    readbacks: &'v [Seq],
}

impl Authorized<'_> {
    pub(crate) const fn readbacks(&self) -> &[Seq] {
        self.readbacks
    }
}

pub(crate) fn authorize(verified: &Verified) -> Authorized<'_> {
    Authorized {
        readbacks: verified.readbacks(),
    }
}

/// Starts a trial: the only caller of the seam's `start`, which takes the proof that nothing else runs and gives it
/// back only with a failed start whose rollback confirmed it.
pub(crate) fn start(
    trials: &mut dyn Trials,
    cancel: &CancellationToken,
    _authorized: Authorized<'_>,
    spec: TrialSpec,
    ready: Stopped,
) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
    trials.start(cancel, spec, ready)
}

/// Journals what a running trial reports, each event citing the trial's start. The first append that fails is kept,
/// later reports are dropped, and the trial is cancelled.
pub(crate) struct Journaled<'a> {
    appender: &'a mut Appender,
    trial: &'a TrialId,
    start: Seq,
    cancel: &'a CancellationToken,
    failure: Option<AppendFailure>,
}

impl<'a> Journaled<'a> {
    pub(crate) fn new(
        appender: &'a mut Appender,
        trial: &'a TrialId,
        start: Seq,
        cancel: &'a CancellationToken,
    ) -> Self {
        Self {
            appender,
            trial,
            start,
            cancel,
            failure: None,
        }
    }

    pub(crate) fn into_failure(self) -> Option<AppendFailure> {
        self.failure
    }

    fn record(&mut self, payload: Payload) {
        if self.failure.is_some() {
            return;
        }
        if let Err(failure) = self.appender.append(payload, &[self.start]) {
            self.failure = Some(failure);
            self.cancel.cancel();
        }
    }
}

impl Reporter for Journaled<'_> {
    fn progress(&mut self, detail: &str) {
        self.record(Payload::TrialProgress(TrialProgress {
            trial: self.trial.clone(),
            detail: detail.to_owned(),
            signal: None,
            core: None,
        }));
    }

    fn sample(&mut self, sample: Sample) {
        self.record(Payload::TrialSample(TrialSample {
            trial: self.trial.clone(),
            warning: sample.warning,
            pid: sample.pid,
            tid: sample.tid,
            cpu: sample.cpu,
        }));
    }

    fn signal(&mut self, core: CoreId, signal: Signal, detail: &str) {
        let label = match signal {
            Signal::ComputationError => "computation error",
            Signal::UnexpectedExit => "backend exited early",
            Signal::Stall => "backend stalled",
            Signal::CorrectedMce | Signal::UncorrectedMce | Signal::Crash => signal.as_str(),
        };
        let named = !matches!(signal, Signal::CorrectedMce | Signal::UncorrectedMce);
        self.record(Payload::TrialProgress(TrialProgress {
            trial: self.trial.clone(),
            detail: format!("core {core:02} {label}: {detail}"),
            signal: Some(signal),
            core: named.then_some(core),
        }));
    }
}

#[cfg(test)]
mod tests;
