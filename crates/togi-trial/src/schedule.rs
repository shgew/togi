use std::time::Duration;

use crate::plan::Load;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Stop,
    Cont,
}

/// One scheduled signal, `at` measured from the start of timing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub at: Duration,
    pub instance: usize,
    pub action: Action,
}

pub(crate) const BURST_LENGTH: Duration = Duration::from_millis(100);
pub(crate) const BURST_PERIOD: Duration = Duration::from_secs(2);

/// The signals a load shape sends before `duration`, lazily and in time order.
pub(crate) fn timeline(load: &Load, duration: Duration) -> Box<dyn Iterator<Item = Step>> {
    let _ = (load, duration);
    unimplemented!()
}
