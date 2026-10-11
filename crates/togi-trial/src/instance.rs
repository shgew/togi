use std::fs::File;
use std::io;
use std::rc::Rc;

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::evidence::Stream;
use crate::id::ScopeName;
use crate::os::{Os, Pipe};
use crate::plan::InstancePlan;
use crate::watch::Watched;

/// What a pipe reader tells the supervisor.
#[derive(Debug)]
pub(crate) enum Inbound {
    Line {
        instance: usize,
        stream: Stream,
        text: String,
    },
    /// The reader ended: end of input, a read failure, or an oversized line.
    Closed {
        instance: usize,
        stream: Stream,
        end: PipeEnd,
    },
    Exited {
        instance: usize,
    },
}

#[derive(Debug)]
pub(crate) enum PipeEnd {
    /// End of input; the unterminated final line, unless the line was oversized.
    Eof { partial: Option<String> },
    Exceeded,
    Failed(io::Error),
    LogWrite(io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Run {
    Running,
    Suspended,
}

/// One pipe's reader task; it owns the retained log descriptor and ends at end of input.
pub(crate) struct Reader {
    pub(crate) stream: Stream,
    pub(crate) task: JoinHandle<()>,
    pub(crate) closed: bool,
}

impl Reader {
    /// Reads in 4 KiB chunks, frames lines, appends every byte to `log`, and sends events.
    pub(crate) fn spawn(
        instance: usize,
        stream: Stream,
        pipe: Pipe,
        log: File,
        events: UnboundedSender<Inbound>,
    ) -> Self {
        let _ = (instance, stream, pipe, log, events);
        unimplemented!()
    }
}

/// A launched instance and everything its supervisor owns about it.
pub(crate) struct LiveInstance<O: Os> {
    pub(crate) index: usize,
    pub(crate) plan: InstancePlan,
    pub(crate) scope: ScopeName,
    pub(crate) pid: u32,
    pub(crate) child: Rc<O::Child>,
    pub(crate) run: Run,
    pub(crate) waiter: JoinHandle<()>,
    pub(crate) readers: [Reader; 2],
    pub(crate) watched: Vec<Watched>,
}
