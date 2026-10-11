use std::sync::Arc;
use std::sync::mpsc::Sender;

use togi_backend::Backend;

use crate::evidence::{Decision, Observation, Report, Selector, Stream, Verdict};
use crate::supervisor::Event;

/// Classifies output with the backend, feeds the one selector and forwards what it decides to
/// report to the thread blocked in `wait`.
pub(crate) struct Judge {
    selector: Selector,
    backend: Arc<dyn Backend>,
    events: Sender<Event>,
}

impl Judge {
    pub(crate) fn new(
        selector: Selector,
        backend: Arc<dyn Backend>,
        events: Sender<Event>,
    ) -> Self {
        Self {
            selector,
            backend,
            events,
        }
    }

    /// Returns whether the evidence says the trial must end.
    pub(crate) fn observe(&mut self, observation: Observation) -> bool {
        let decision = self.selector.observe(observation);
        self.forward(decision)
    }

    pub(crate) fn line(&mut self, instance: usize, pid: u32, stream: Stream, text: String) -> bool {
        let line = self.backend.classify(&text);
        self.observe(Observation::Line {
            instance,
            pid,
            stream,
            line,
            text,
        })
    }

    fn forward(&self, decision: Decision) -> bool {
        for report in decision.reports {
            let _ = self.events.send(report_event(report));
        }
        decision.stop
    }

    /// Settles what the exits seen before teardown mean, once final output was classified.
    pub(crate) fn conclude(self) -> Verdict {
        let Self {
            selector, events, ..
        } = self;
        let (verdict, reports) = selector.conclude();
        for report in reports {
            let _ = events.send(report_event(report));
        }
        verdict
    }
}

fn report_event(report: Report) -> Event {
    match report {
        Report::Progress(detail) => Event::Progress(detail),
        Report::Warning(sample) => Event::Sample(sample),
        Report::Signal {
            core,
            signal,
            detail,
        } => Event::Signal {
            core,
            signal,
            detail,
        },
    }
}
