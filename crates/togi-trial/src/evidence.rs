use togi_backend::Line;
use togi_machine::{CoreId, CpuId, Sample, Signal, TrialFailure};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
    Watched,
}

/// What the supervisor saw, in the order it saw it; instances are indices into the trial plan.
#[derive(Debug)]
pub(crate) enum Observation {
    Line {
        instance: usize,
        stream: Stream,
        line: Line,
        text: String,
    },
    UnexpectedExit {
        instance: usize,
        detail: String,
    },
    Escape {
        instance: usize,
        cpu: CpuId,
        pid: u32,
        tid: u32,
    },
    Stall {
        instance: usize,
        detail: String,
    },
    LineTooLong {
        instance: usize,
        stream: Stream,
    },
    OutputError {
        instance: usize,
        stream: Stream,
        error: String,
    },
    SamplingLoss {
        instance: usize,
        what: String,
    },
    NeverEntered {
        instance: usize,
        scope: String,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) enum Report {
    Progress(String),
    Warning(Sample),
    Signal {
        core: CoreId,
        signal: Signal,
        detail: String,
    },
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Decision {
    pub(crate) stop: bool,
    pub(crate) reports: Vec<Report>,
}

/// What the evidence concluded; the trial's result is built from it.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Verdict {
    pub(crate) failure: Option<TrialFailure>,
    pub(crate) inconclusive: Option<String>,
    pub(crate) escaped: Vec<CpuId>,
}

/// The one selector, for live and teardown observations alike. Precedence: escape, then
/// computation error, stall or unexpected exit (first selected core), then setup error, then
/// sampling or observation loss. Oversized-line errors are suppressed only by stronger evidence.
#[derive(Debug)]
pub(crate) struct Selector {
    cores: Vec<CoreId>,
}

impl Selector {
    pub(crate) fn new(cores: Vec<CoreId>) -> Self {
        Self { cores }
    }

    pub(crate) fn observe(&mut self, observation: Observation) -> Decision {
        let _ = (observation, &self.cores);
        unimplemented!()
    }

    /// Whether a computation error, escape or other stronger evidence than `signal` is selected.
    pub(crate) fn selected(&self) -> Option<TrialFailure> {
        unimplemented!()
    }

    pub(crate) fn conclude(self) -> Verdict {
        unimplemented!()
    }
}
