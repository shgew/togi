use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub(crate) const COMMAND_DEADLINE: Duration = Duration::from_secs(1);

/// The RSMU commands togi sends.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Command {
    ReadOffset,
    SetOffset,
    SetAllOffsets,
    BoostLimit,
}

impl Command {
    pub const fn code(self) -> u32 {
        match self {
            Self::ReadOffset => 0xd5,
            Self::SetOffset => 0x06,
            Self::SetAllOffsets => 0x07,
            Self::BoostLimit => 0x6e,
        }
    }
}

/// The status the driver reports for a command that was not acknowledged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Failed,
    UnknownCommand,
    Prerequisite,
    Busy,
    Unknown(u32),
}

impl Status {
    const ACKNOWLEDGED: u32 = 1;

    pub(crate) fn check(code: u32) -> Result<(), Self> {
        match code {
            Self::ACKNOWLEDGED => Ok(()),
            0xff => Err(Self::Failed),
            0xfe => Err(Self::UnknownCommand),
            0xfd => Err(Self::Prerequisite),
            0xfc => Err(Self::Busy),
            other => Err(Self::Unknown(other)),
        }
    }

    pub const fn code(self) -> u32 {
        match self {
            Self::Failed => 0xff,
            Self::UnknownCommand => 0xfe,
            Self::Prerequisite => 0xfd,
            Self::Busy => 0xfc,
            Self::Unknown(code) => code,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Failed => "failed",
            Self::UnknownCommand => "unknown command",
            Self::Prerequisite => "rejected: prerequisite",
            Self::Busy => "rejected: busy",
            Self::Unknown(_) => "unknown status",
        };
        write!(f, "{:#04x} ({name})", self.code())
    }
}

/// One mailbox transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Request {
    Command(Command),
    ReadSmn(u32),
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(command) => write!(f, "RSMU command {:#x}", command.code()),
            Self::ReadSmn(address) => write!(f, "SMN read {address:#x}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MailboxError {
    #[error("open {}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("short write to {}: {written} of {expected} bytes", path.display())]
    ShortWrite {
        path: PathBuf,
        written: usize,
        expected: usize,
    },
    #[error("read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read {}: got {got} bytes, want {want}", path.display())]
    Length {
        path: PathBuf,
        got: usize,
        want: usize,
    },
    #[error("RSMU command {command:#x}: status {status}")]
    Status { command: u32, status: Status },
    #[error("{request} did not finish within {deadline:?}; the driver refuses further commands")]
    TimedOut {
        request: Request,
        deadline: Duration,
    },
    #[error("an earlier mailbox transaction timed out; the driver refuses further commands")]
    Poisoned,
    #[error("the mailbox worker stopped")]
    WorkerLost,
}

/// The hardware side of the mailbox: one blocking transaction at a time.
pub(crate) trait Transport: Send {
    fn command(&mut self, command: Command, args: [u32; 6]) -> Result<[u32; 6], MailboxError>;
    fn read_smn(&mut self, address: u32) -> Result<u32, MailboxError>;
}

type Job = Box<dyn FnOnce(&mut dyn Transport) + Send>;

/// Runs every transaction on one thread and gives each a deadline.
///
/// A blocked transaction cannot be cancelled, so an overrun poisons the worker for good: no
/// later transaction starts while the stuck one may still be running.
pub(crate) struct Worker {
    jobs: mpsc::Sender<Job>,
    deadline: Duration,
    poisoned: bool,
}

impl Worker {
    pub(crate) fn spawn(mut transport: Box<dyn Transport>, deadline: Duration) -> io::Result<Self> {
        let (jobs, queue) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("togi-smu-mailbox".to_owned())
            .spawn(move || {
                for job in queue {
                    job(&mut *transport);
                }
            })?;
        Ok(Self {
            jobs,
            deadline,
            poisoned: false,
        })
    }

    fn run<T: Send + 'static>(
        &mut self,
        request: Request,
        transaction: impl FnOnce(&mut dyn Transport) -> Result<T, MailboxError> + Send + 'static,
    ) -> Result<T, MailboxError> {
        if self.poisoned {
            return Err(MailboxError::Poisoned);
        }
        let (reply, answer) = mpsc::channel();
        let job: Job = Box::new(move |transport| {
            let _ = reply.send(transaction(transport));
        });
        if self.jobs.send(job).is_err() {
            self.poisoned = true;
            return Err(MailboxError::WorkerLost);
        }
        match answer.recv_timeout(self.deadline) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.poisoned = true;
                Err(MailboxError::TimedOut {
                    request,
                    deadline: self.deadline,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.poisoned = true;
                Err(MailboxError::WorkerLost)
            }
        }
    }

    pub(crate) fn command(
        &mut self,
        command: Command,
        args: [u32; 6],
    ) -> Result<[u32; 6], MailboxError> {
        self.run(Request::Command(command), move |transport| {
            transport.command(command, args)
        })
    }

    pub(crate) fn read_smn(&mut self, address: u32) -> Result<u32, MailboxError> {
        self.run(Request::ReadSmn(address), move |transport| {
            transport.read_smn(address)
        })
    }
}
