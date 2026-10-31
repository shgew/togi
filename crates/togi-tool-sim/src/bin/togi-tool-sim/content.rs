//! `--recorded-content`: simulated trials report the content identities a journal recorded instead of the
//! simulator's synthetic ones, so a transition dry run stays evidence-compatible with the recording.
//!
//! The identities are a simulation assumption read from the journal. Nothing here proves that the executables
//! they name ran, or exist, on this machine.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::rc::Rc;

use togi_journal::{Event, Payload, Seq, WorkloadContent};
use togi_machine::{
    Backend, CancellationToken, ContentId, Error, Partial, Running, SampleError, Stopped, Swept,
    TrialConditions, TrialSpec, Trials, WorkloadId,
};
use togi_simrun::TrialsAdapter;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RecordedContentError {
    #[error("the state directory's journal records no workload content identity")]
    NoneRecorded,
    #[error(
        "event {seq} records {recorded:?} content for workload {workload}, which runs {expected:?}"
    )]
    Backend {
        seq: Seq,
        workload: WorkloadId,
        recorded: Backend,
        expected: Backend,
    },
    #[error(
        "events {first} and {second} record different content identities for workload {workload}"
    )]
    Conflict {
        workload: WorkloadId,
        first: Seq,
        second: Seq,
    },
    #[error("the journal records no content identity for workload {0}")]
    Missing(WorkloadId),
}

/// The content identity each workload that runs recorded, keyed as the session asks for it, by what it runs.
#[derive(Debug)]
pub(crate) struct RecordedContent {
    pub(crate) contents: BTreeMap<WorkloadId, ContentId>,
}

impl RecordedContent {
    /// Reads the identities of `events`: every workload an exposure's class or a trial intent names, with the
    /// content it was frozen with. Equal repeats agree; a workload with two identities, or one on the wrong
    /// backend, is refused rather than resolved by order.
    pub(crate) fn from_events(events: &[Event]) -> Result<Self, RecordedContentError> {
        let mut found: BTreeMap<WorkloadId, (Seq, ContentId)> = BTreeMap::new();
        for event in events {
            for recorded in recorded(&event.payload) {
                let workload = recorded.workload.runs();
                let expected = workload.backend();
                if recorded.content.backend != expected {
                    return Err(RecordedContentError::Backend {
                        seq: event.seq,
                        workload: recorded.workload,
                        recorded: recorded.content.backend,
                        expected,
                    });
                }
                match found.entry(workload) {
                    Entry::Vacant(slot) => {
                        slot.insert((event.seq, recorded.content.clone()));
                    }
                    Entry::Occupied(slot) => {
                        let (first, content) = slot.get();
                        if *content != recorded.content {
                            return Err(RecordedContentError::Conflict {
                                workload,
                                first: *first,
                                second: event.seq,
                            });
                        }
                    }
                }
            }
        }
        if found.is_empty() {
            return Err(RecordedContentError::NoneRecorded);
        }
        Ok(Self {
            contents: found
                .into_iter()
                .map(|(workload, (_, content))| (workload, content))
                .collect(),
        })
    }

    /// Wraps each boot's simulated trial runner so its `content` answers with the recorded identities.
    pub(crate) fn adapter(self) -> TrialsAdapter {
        let contents = Rc::new(self.contents);
        Box::new(move |inner: Box<dyn Trials>| -> Box<dyn Trials> {
            Box::new(RecordedTrials {
                inner,
                contents: Rc::clone(&contents),
            })
        })
    }
}

/// Every workload identity a journal event records.
pub(crate) fn recorded(payload: &Payload) -> &[WorkloadContent] {
    if let Payload::ExposureObserved(observed) = payload {
        &observed.class.workloads
    } else if let Payload::TrialIntent(intent) = payload {
        &intent.content
    } else {
        &[]
    }
}

/// The simulator's trial runner, unchanged but for the identity `content` reports.
struct RecordedTrials {
    inner: Box<dyn Trials>,
    contents: Rc<BTreeMap<WorkloadId, ContentId>>,
}

impl Trials for RecordedTrials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        self.inner.start(cancel, spec, ready)
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)
    }

    /// The backend's availability is still the simulator's to decide; only the identity it would report is
    /// replaced. A workload without a recorded identity is refused, never given a synthetic one.
    fn content(&mut self, workload: WorkloadId) -> Result<ContentId, Error> {
        self.inner.content(workload)?;
        self.contents
            .get(&workload.runs())
            .cloned()
            .ok_or_else(|| Error::Other(Box::new(RecordedContentError::Missing(workload))))
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, SampleError>> + 'a> {
        self.inner.samples(id)
    }

    fn controller_pid(&self) -> u32 {
        self.inner.controller_pid()
    }
}
