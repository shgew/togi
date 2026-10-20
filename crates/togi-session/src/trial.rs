//! Starting a trial and journaling what it reports.

use togi_journal::{Payload, Seq, TrialId, TrialProgress, TrialSample};
use togi_machine::{CancellationToken, CoreId, Reporter, Sample, Signal};

use crate::appender::Appender;
use crate::error::AppendFailure;

pub(crate) use crate::write::authority::{authorize, start};

/// Journals what a running trial reports, each event citing the trial's start. The first append that fails is kept,
/// later reports are dropped, and the trial is cancelled.
pub(crate) struct Journaled<'a, 'machine> {
    appender: &'a mut Appender<'machine>,
    trial: &'a TrialId,
    start: Seq,
    cancel: &'a CancellationToken,
    failure: Option<AppendFailure>,
}

impl<'a, 'machine> Journaled<'a, 'machine> {
    pub(crate) fn new(
        appender: &'a mut Appender<'machine>,
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

impl Reporter for Journaled<'_, '_> {
    fn progress(&mut self, detail: &str) {
        self.record(Payload::TrialProgress(TrialProgress {
            trial: self.trial.clone(),
            detail: detail.to_owned(),
            signal: None,
            core: None,
            fault: None,
            pid: None,
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

    fn signal(&mut self, core: CoreId, signal: Signal, pid: Option<u32>, detail: &str) {
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
            fault: None,
            pid,
        }));
    }
}

#[cfg(test)]
mod tests;
