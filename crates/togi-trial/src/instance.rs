use std::io;
use std::rc::Rc;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::CleanupFailure;
use crate::evidence::{Observation, Stream};
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
        partial: Option<String>,
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
}

impl Reader {
    /// A reader that never existed because the pipes could not be taken; the failure that caused it
    /// is recorded separately, so its closing never confirms a drain.
    pub(crate) fn absent() -> Self {
        Self {
            task: tokio::task::spawn_local(async {}),
            closed: true,
        }
    }

    /// Reads in 4 KiB chunks, frames lines, queues every consumed byte for `log`, and reports.
    pub(crate) fn spawn(
        instance: usize,
        stream: Stream,
        mut pipe: Pipe,
        mut log: LogHandle,
        events: Sender<Inbound>,
    ) -> Self {
        let task = tokio::task::spawn_local(async move {
            let mut framer = Framer::default();
            let mut buffer = [0_u8; CHUNK];
            let end = loop {
                let count = match pipe.read(&mut buffer).await {
                    Ok(0) => {
                        break PipeEnd::Eof {
                            partial: framer.finish(),
                        };
                    }
                    Ok(count) => count,
                    Err(error) => break PipeEnd::Failed(error),
                };
                let mut lines = Vec::new();
                let consumed = framer.push(&buffer[..count], &mut |frame| {
                    if let Frame::Line(text) = frame {
                        lines.push(text);
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
                for text in lines {
                    let event = Inbound::Line {
                        instance,
                        stream,
                        text,
                    };
                    if events.send(event).await.is_err() {
                        return;
                    }
                }
                if framer.exceeded() {
                    break PipeEnd::Exceeded;
                }
            };
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
        } => stop(judge.line(instance, instances[instance].pid, stream, text)),
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
                PipeEnd::Eof { partial } => {
                    stop(partial.is_some_and(|text| judge.line(instance, pid, stream, text)))
                }
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
            .poll(
                until,
                &mut |line| {
                    if judge.line(index, pid, Stream::Watched, line.to_owned()) {
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
                if judge.line(index, pid, Stream::Watched, line.to_owned()) {
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
