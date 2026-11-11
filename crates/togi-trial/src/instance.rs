use std::cell::RefCell;
use std::future::poll_fn;
use std::io;
use std::rc::{Rc, Weak};
use std::task::Poll as TaskPoll;
use std::time::Duration;

use tokio::io::ReadBuf;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::CleanupFailure;
use crate::evidence::{Observation, Stream, Verification};
use crate::id::ScopeName;
use crate::judge::Judge;
use crate::logs::LogHandle;
use crate::os::{Child, Os, Pipe};
use crate::output::{CHUNK, Frame, Framer};
use crate::plan::InstancePlan;
use crate::stall::Stall;
use crate::teardown::Failures;
use crate::watch::{Poll, WatchError, Watched, WritersStopped};

/// What readers and waiters tell the supervisor.
#[derive(Debug)]
pub(crate) enum Inbound {
    Line {
        instance: usize,
        stream: Stream,
        text: String,
        verification: Verification,
    },
    /// The log file could not take bytes; reading goes on.
    LogError {
        instance: usize,
        stream: Stream,
        error: io::Error,
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
    Eof {
        partial: Option<(String, Verification)>,
    },
    Exceeded,
    Failed(io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Run {
    Running,
    Suspended,
}

/// One pipe's reader task; it queues the bytes for its log and ends at end of input.
pub(crate) struct Reader {
    pub(crate) task: JoinHandle<()>,
    pub(crate) closed: bool,
    /// Only the task owns the pipe: keeping the start fence must not keep a finished read end open.
    capture: Weak<RefCell<Capture>>,
}

#[derive(Clone, Copy)]
enum Interval {
    Startup,
    Measured {
        cutoff: u64,
        deadline: Instant,
    },
    /// A failed backlog snapshot cannot establish when any later bytes were produced.
    Uncertain,
}

struct Capture {
    pipe: Pipe,
    consumed: u64,
    interval: Interval,
}

impl Capture {
    fn begin(&mut self, deadline: Instant) {
        self.interval = match self.pipe.as_ref().get_ref().unread() {
            Ok(queued) => Interval::Measured {
                cutoff: self.consumed + queued,
                deadline,
            },
            Err(_) => Interval::Uncertain,
        };
    }

    async fn read(capture: &RefCell<Self>, buffer: &mut [u8]) -> io::Result<(usize, Verification)> {
        poll_fn(|cx| {
            let mut capture = capture.borrow_mut();
            let mut buf = ReadBuf::new(buffer);
            match tokio::io::AsyncRead::poll_read(capture.pipe.as_mut(), cx, &mut buf) {
                TaskPoll::Ready(Ok(())) => {
                    let count = buf.filled().len();
                    let verification = match capture.interval {
                        Interval::Measured { cutoff, deadline }
                            if capture.consumed >= cutoff && Instant::now() < deadline =>
                        {
                            Verification::Measured
                        }
                        Interval::Startup | Interval::Measured { .. } | Interval::Uncertain => {
                            Verification::Outside
                        }
                    };
                    capture.consumed += count as u64;
                    TaskPoll::Ready(Ok((count, verification)))
                }
                TaskPoll::Ready(Err(error)) => TaskPoll::Ready(Err(error)),
                TaskPoll::Pending => TaskPoll::Pending,
            }
        })
        .await
    }
}

impl Reader {
    /// A reader that never existed because the pipes could not be taken; the failure that caused it
    /// is recorded separately, so its closing never confirms a drain.
    pub(crate) fn absent() -> Self {
        Self {
            task: tokio::task::spawn_local(async {}),
            closed: true,
            capture: Weak::new(),
        }
    }

    /// Snapshot after the measured start: bytes already read, queued or still in the kernel pipe
    /// remain classifiable but cannot verify a core. A later snapshot only discards extra bytes.
    pub(crate) fn begin_interval(&self, deadline: Instant) {
        if let Some(capture) = self.capture.upgrade() {
            capture.borrow_mut().begin(deadline);
        }
    }

    /// Reads in 4 KiB chunks, frames lines, queues every consumed byte for `log`, and reports.
    pub(crate) fn spawn(
        instance: usize,
        stream: Stream,
        pipe: Pipe,
        mut log: LogHandle,
        events: Sender<Inbound>,
    ) -> Self {
        let capture = Rc::new(RefCell::new(Capture {
            pipe,
            consumed: 0,
            interval: Interval::Startup,
        }));
        let reading = Rc::clone(&capture);
        let task = tokio::task::spawn_local(async move {
            let mut framer = Framer::default();
            let mut buffer = [0_u8; CHUNK];
            let mut partial = Verification::Outside;
            let end = loop {
                let (count, verification) = match Capture::read(&reading, &mut buffer).await {
                    Ok((0, verification)) => {
                        break PipeEnd::Eof {
                            partial: framer.finish().map(|text| {
                                let stamp = if verification == Verification::Measured {
                                    partial
                                } else {
                                    Verification::Outside
                                };
                                (text, stamp)
                            }),
                        };
                    }
                    Ok(read) => read,
                    Err(error) => break PipeEnd::Failed(error),
                };
                if !framer.pending() || verification == Verification::Outside {
                    partial = verification;
                }
                let mut lines = Vec::new();
                let consumed = framer.push(&buffer[..count], &mut |frame| {
                    if let Frame::Line(text) = frame {
                        lines.push((text, partial));
                        partial = verification;
                    }
                    std::ops::ControlFlow::Continue(())
                });
                if log.append(&buffer[..consumed]).is_err() {
                    let event = Inbound::LogError {
                        instance,
                        stream,
                        error: io::Error::other(
                            "the log writer fell behind; output bytes were dropped",
                        ),
                    };
                    if events.send(event).await.is_err() {
                        return;
                    }
                }
                for (text, verification) in lines {
                    let event = Inbound::Line {
                        instance,
                        stream,
                        text,
                        verification,
                    };
                    if events.send(event).await.is_err() {
                        return;
                    }
                }
                if framer.exceeded() {
                    break PipeEnd::Exceeded;
                }
            };
            // Closing the read end releases a writer blocked in a large write, so it can still
            // report stronger failure evidence on its other stream during teardown.
            drop(reading);
            let _ = events
                .send(Inbound::Closed {
                    instance,
                    stream,
                    end,
                })
                .await;
        });
        Self {
            task,
            closed: false,
            capture: Rc::downgrade(&capture),
        }
    }
}

/// What the supervisor observes of one instance between ticks.
pub(crate) struct Observed {
    pub(crate) run: Run,
    pub(crate) ready: bool,
    pub(crate) exited: bool,
    /// Unsuspended time accumulated before the current running spell.
    pub(crate) active: Duration,
    pub(crate) resumed: Instant,
    pub(crate) stall: Stall,
}

impl Observed {
    pub(crate) fn active_at(&self, now: Instant) -> Duration {
        match self.run {
            Run::Running => self.active + now.saturating_duration_since(self.resumed),
            Run::Suspended => self.active,
        }
    }

    pub(crate) fn suspend(&mut self, now: Instant) {
        if self.run == Run::Running {
            self.active += now.saturating_duration_since(self.resumed);
            self.run = Run::Suspended;
        }
    }

    pub(crate) fn resume(&mut self, now: Instant) {
        if self.run == Run::Suspended {
            self.resumed = now;
            self.run = Run::Running;
        }
    }
}

/// A launched instance and everything its supervisor owns about it.
pub(crate) struct LiveInstance<O: Os> {
    pub(crate) index: usize,
    pub(crate) plan: InstancePlan,
    pub(crate) scope: ScopeName,
    pub(crate) pid: u32,
    /// Present until teardown reaps the leader.
    pub(crate) child: Option<Rc<O::Child>>,
    pub(crate) waiter: JoinHandle<()>,
    pub(crate) readers: [Reader; 2],
    pub(crate) watched: Vec<Watched>,
    pub(crate) writers: Option<WritersStopped>,
    pub(crate) observed: Observed,
}

impl<O: Os> LiveInstance<O> {
    pub(crate) fn readers_closed(&self) -> bool {
        self.readers.iter().all(|reader| reader.closed)
    }

    pub(crate) fn exited(&self) -> bool {
        self.observed.exited
    }

    pub(crate) fn signal(
        &self,
        signal: crate::os::GroupSignal,
    ) -> Result<(), crate::os::SignalError> {
        match &self.child {
            Some(child) => child.signal_group(signal),
            None => Err(crate::os::SignalError::Gone),
        }
    }
}

/// What handling one inbound event decided.
#[derive(Clone, Copy)]
pub(crate) enum Flow {
    Go,
    Stop,
    /// The launcher exited; the caller decides whether that is expected.
    Exited(usize),
}

/// Applies one inbound event to the judge and the instance state; read failures are cleanup
/// failures that teardown must keep.
pub(crate) fn dispatch<O: Os>(
    judge: &mut Judge,
    instances: &mut [LiveInstance<O>],
    failures: &mut Failures,
    event: Inbound,
) -> Flow {
    let stop = |stop: bool| if stop { Flow::Stop } else { Flow::Go };
    match event {
        Inbound::Line {
            instance,
            stream,
            text,
            verification,
        } => stop(judge.line(
            instance,
            instances[instance].pid,
            stream,
            verification,
            text,
        )),
        Inbound::LogError {
            instance,
            stream,
            error,
        } => stop(judge.observe(Observation::OutputError {
            instance,
            stream,
            error: format!("write output log: {error}"),
        })),
        Inbound::Closed {
            instance,
            stream,
            end,
        } => {
            let inst = &mut instances[instance];
            let slot = match stream {
                Stream::Stderr => 1,
                Stream::Stdout | Stream::Watched => 0,
            };
            inst.readers[slot].closed = true;
            let pid = inst.pid;
            let label = inst.plan.name.dir_name();
            match end {
                PipeEnd::Eof { partial } => stop(partial.is_some_and(|(text, verification)| {
                    judge.line(instance, pid, stream, verification, text)
                })),
                PipeEnd::Exceeded => {
                    stop(judge.observe(Observation::LineTooLong { instance, stream }))
                }
                PipeEnd::Failed(error) => {
                    let message = format!("read backend output: {error}");
                    failures.record(CleanupFailure::ReadPipe {
                        instance: label,
                        stream: stream.name(),
                        source: error,
                    });
                    stop(judge.observe(Observation::OutputError {
                        instance,
                        stream,
                        error: message,
                    }))
                }
            }
        }
        Inbound::Exited { instance } => {
            instances[instance].observed.exited = true;
            Flow::Exited(instance)
        }
    }
}

/// What one pass over an instance's watched files found.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Polled {
    /// Bytes remain unread.
    pub(crate) pending: bool,
    /// Evidence says the trial must end.
    pub(crate) stop: bool,
}

/// Polls every watched file of `instance` once. The wait for each file's blocking read ends at
/// `until`; `halt` ends a poll early between chunks and lines, and a line that ends the trial
/// ends it between lines.
pub(crate) async fn poll_watched<O: Os>(
    instance: &mut LiveInstance<O>,
    judge: &mut Judge,
    failures: &mut Failures,
    until: Instant,
    halt: &mut impl FnMut() -> bool,
) -> Polled {
    let (index, pid) = (instance.index, instance.pid);
    let label = instance.plan.name.dir_name();
    let mut polled = Polled::default();
    for file in &mut instance.watched {
        let mut ended = false;
        let poll = file
            .poll_interval(
                until,
                judge.deadline(),
                &mut |line, verification| {
                    if judge.line(index, pid, Stream::Watched, verification, line.to_owned()) {
                        ended = true;
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                },
                halt,
            )
            .await;
        polled.stop |= ended;
        match poll {
            Poll::Idle | Poll::Drained => {}
            Poll::Pending => polled.pending = true,
            Poll::Failed(error) => {
                polled.stop |= report(judge, failures, index, &label, &error);
            }
        }
    }
    polled
}

/// The last read of every watched file of `instance` after its writers were confirmed stopped:
/// whatever they appended since the last poll is read and classified, then the unterminated
/// final line. A file that cannot be read to its end stays undrained, or fails as in a poll.
/// Does nothing for an instance whose writers are not confirmed.
pub(crate) async fn finish_watched<O: Os>(
    instance: &mut LiveInstance<O>,
    judge: &mut Judge,
    failures: &mut Failures,
    until: Instant,
) {
    let Some(writers) = instance.writers else {
        return;
    };
    let (index, pid) = (instance.index, instance.pid);
    let label = instance.plan.name.dir_name();
    for file in &mut instance.watched {
        let poll = file
            .finish(writers, until, &mut |line| {
                if judge.line(
                    index,
                    pid,
                    Stream::Watched,
                    Verification::Outside,
                    line.to_owned(),
                ) {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            })
            .await;
        if let Poll::Failed(error) = poll {
            report(judge, failures, index, &label, &error);
        }
    }
}

/// Records a watched file's end: containment when accepted bytes were lost, an evidence
/// observation always. Whether that observation ends the trial.
fn report(
    judge: &mut Judge,
    failures: &mut Failures,
    index: usize,
    label: &str,
    error: &WatchError,
) -> bool {
    let observation = match error {
        WatchError::Missing => Observation::SamplingLoss {
            instance: index,
            what: error.to_string(),
        },
        WatchError::LineTooLong => Observation::LineTooLong {
            instance: index,
            stream: Stream::Watched,
        },
        WatchError::Lost => {
            failures.record(CleanupFailure::WatchedLost {
                instance: label.to_owned(),
            });
            Observation::OutputError {
                instance: index,
                stream: Stream::Watched,
                error: error.to_string(),
            }
        }
        WatchError::Unsupported | WatchError::Refused(_) | WatchError::Read(_) => {
            Observation::OutputError {
                instance: index,
                stream: Stream::Watched,
                error: error.to_string(),
            }
        }
    };
    judge.observe(observation)
}
