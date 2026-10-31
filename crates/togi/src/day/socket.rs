//! Local, credential-checked transport for day questions and crash records. Only the observer owns the journal
//! writer.

use std::io::{self, BufRead as _, Read as _, Write as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use togi_journal::{
    DayAnswer, DayAnswerKind, DayQuestions, DayRecord, DayUncleanEnd, Journal, PauseState, Seq,
    SessionId,
};
use togi_machine::{BootId, Clock, object_only};
use togi_session::{AppendRecordError, append_record};

/// An authenticated control operation. Identity always comes from the socket credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Status,
    Pause {
        state: PauseState,
        uid: u32,
    },
    /// Extends the timed pause that `revision` names by one hour; refused when another change followed it.
    Extend {
        revision: Seq,
        uid: u32,
    },
    Resume {
        uid: u32,
    },
    Freeze {
        frozen: bool,
        uid: u32,
    },
    Reapply,
}

/// The observer acknowledges a control only after its durable transaction succeeds.
pub type Controller<'a> =
    &'a mut dyn FnMut(&mut Journal, &dyn Clock, Control) -> Result<ControlStatus, super::Error>;

/// Incoming controls for the live observation loop.
pub trait ControlSource {
    /// Polls without waiting for a request and acknowledges only completed transactions.
    ///
    /// # Errors
    /// Returns a durable control or transport failure that must stop the service.
    fn poll(
        &self,
        journal: &mut Journal,
        clock: &dyn Clock,
        control: Controller<'_>,
    ) -> Result<(), super::Error>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlStatus {
    pub pause: PauseState,
    pub frozen: bool,
    /// Newest `day.pause` event, used to deduplicate desktop notifications.
    pub revision: Option<Seq>,
}

pub const PATH: &str = "/run/togi/day.sock";
/// Room for a crash record of every core with a long note; the deadline, not this, bounds a slow peer.
const LIMIT: u64 = 16 * 1024;
/// Total time one client may hold the observer loop, matching its one-second tick.
const EXCHANGE: Duration = Duration::from_secs(1);

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "one request per connection, decoded once; boxing the record would only add an allocation"
)]
pub enum Request {
    Questions,
    State,
    Pause(PauseRequest),
    /// Notifier only: extend the timed pause that `revision` names, so a resume, expiry or replacement since the
    /// notifier read the state is refused instead of overwritten.
    Extend(ExtendRequest),
    Resume,
    Freeze(FreezeRequest),
    Answer {
        boot: BootId,
        answer: DayAnswerKind,
    },
    /// A crash record the user confirmed, drafted against `session`; the service appends it only when its live
    /// journal is that session, after checking it against that journal.
    Record {
        session: SessionId,
        record: DayRecord,
    },
    /// Root only: reapply the day profile after resume; answered only once the transaction succeeded.
    Reapply,
}

object_only! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PauseRequest {
        pub state: PauseState,
    }
}

object_only! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ExtendRequest {
        /// The newest `day.pause` the requester saw.
        pub revision: Seq,
    }
}

object_only! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FreezeRequest {
        pub frozen: bool,
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Questions {
        pending: Vec<DayUncleanEnd>,
        morning: Option<crate::morning::Report>,
    },
    State {
        status: ControlStatus,
    },
    Controlled {
        status: ControlStatus,
    },
    Answered,
    Recorded {
        seq: Seq,
    },
    Reapplied,
    Refused {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("access day question socket")]
    Io(#[from] io::Error),
    /// Nothing listens at the socket: no day service runs.
    #[error("reach the day service")]
    Absent(#[source] io::Error),
    #[error("the request is larger than the day service accepts, {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("decode day question protocol")]
    Protocol(#[from] serde_json::Error),
    #[error("day service refused the request: {0}")]
    Refused(String),
}

/// A socket under a root-owned directory; peers cannot replace its pathname.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    group: u32,
}

impl Server {
    /// Bind the production endpoint. The NixOS module supplies the `togi` directory group.
    ///
    /// # Errors
    /// Refuses unsafe directory ownership or non-socket collisions.
    pub fn bind() -> Result<Self, Error> {
        Self::at(Path::new(PATH))
    }

    fn at(path: &Path) -> Result<Self, Error> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("socket has no parent"))?;
        match std::fs::create_dir(parent) {
            Ok(()) => std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o750))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let directory = std::fs::symlink_metadata(parent)?;
        if !directory.is_dir() || directory.uid() != 0 || directory.mode() & 0o022 != 0 {
            return Err(io::Error::other(
                "day socket directory must be root-owned and not group/world writable",
            )
            .into());
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == 0 => {
                std::fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(io::Error::other("day socket path is not a root-owned socket").into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(path)?;
        // The directory already excludes other users while ownership and permissions are set.
        rustix::fs::chown(
            path,
            None,
            Some(rustix::process::Gid::from_raw(directory.gid())),
        )
        .map_err(io::Error::from)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
            group: directory.gid(),
        })
    }

    /// Serve at most one client per tick, bounding an untrusted peer's effect on observation.
    pub fn poll(
        &self,
        journal: &mut Journal,
        clock: &dyn Clock,
        control: Controller<'_>,
    ) -> Result<(), super::Error> {
        let (stream, _) = match self.listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(Error::Io(error).into()),
        };
        let mut exchange = Deadline::new(&stream, clock, EXCHANGE)?;
        let peer = credentials(&stream).and_then(|(uid, gid)| {
            let allowed = if uid == 0 || gid == self.group {
                true
            } else {
                authorized(
                    uid,
                    gid,
                    self.group,
                    &std::fs::read_to_string("/etc/group")?,
                    &std::fs::read_to_string("/etc/passwd")?,
                )
            };
            Ok((uid, allowed))
        });
        let reply = match peer {
            Ok((uid, true)) => match read_request(&mut exchange) {
                Ok(request) => respond(journal, clock, uid, request, control)?,
                Err(error) => Reply::Refused {
                    reason: error.to_string(),
                },
            },
            Ok(_) => Reply::Refused {
                reason: "peer is not a member of the day answer group".into(),
            },
            Err(error) => Reply::Refused {
                reason: error.to_string(),
            },
        };
        // A client disconnect is not an observation failure; durable append errors above are.
        if let Err(_disconnected) = write_reply(&mut exchange, &reply) {}
        Ok(())
    }
}

impl ControlSource for Server {
    fn poll(
        &self,
        journal: &mut Journal,
        clock: &dyn Clock,
        control: Controller<'_>,
    ) -> Result<(), super::Error> {
        self.poll(journal, clock, control)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Err(_unavailable) = std::fs::remove_file(&self.path) {}
    }
}

#[cfg(target_os = "linux")]
fn credentials(stream: &UnixStream) -> io::Result<(u32, u32)> {
    let peer = rustix::net::sockopt::socket_peercred(stream)?;
    Ok((peer.uid.as_raw(), peer.gid.as_raw()))
}

#[cfg(not(target_os = "linux"))]
fn credentials(_stream: &UnixStream) -> io::Result<(u32, u32)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "day answers need Linux peer credentials",
    ))
}

fn authorized(uid: u32, gid: u32, group: u32, groups: &str, passwd: &str) -> bool {
    if uid == 0 || gid == group {
        return true;
    }
    let name = passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        (fields.next()?.parse::<u32>().ok()? == uid).then_some(name)
    });
    name.is_some_and(|name| {
        groups.lines().any(|line| {
            let mut fields = line.split(':');
            let _name = fields.next();
            let _password = fields.next();
            fields.next().and_then(|gid| gid.parse::<u32>().ok()) == Some(group)
                && fields
                    .next()
                    .is_some_and(|names| names.split(',').any(|member| member == name))
        })
    })
}

/// Socket timeouts apply per call; this lets [`Deadline`] shrink them to the remaining budget.
trait Timeouts {
    fn set_read_timeout(&self, timeout: Duration) -> io::Result<()>;
    fn set_write_timeout(&self, timeout: Duration) -> io::Result<()>;
}

impl Timeouts for &UnixStream {
    fn set_read_timeout(&self, timeout: Duration) -> io::Result<()> {
        UnixStream::set_read_timeout(self, Some(timeout))
    }
    fn set_write_timeout(&self, timeout: Duration) -> io::Result<()> {
        UnixStream::set_write_timeout(self, Some(timeout))
    }
}

/// One overall budget for a whole exchange, so a trickling peer cannot hold the observer loop.
struct Deadline<'a, S> {
    stream: S,
    clock: &'a dyn Clock,
    expires: Duration,
}

impl<'a, S> Deadline<'a, S> {
    fn new(stream: S, clock: &'a dyn Clock, budget: Duration) -> Result<Self, Error> {
        let expires = clock.monotonic().map_err(io::Error::other)? + budget;
        Ok(Self {
            stream,
            clock,
            expires,
        })
    }

    fn remaining(&self) -> io::Result<Duration> {
        let now = self.clock.monotonic().map_err(io::Error::other)?;
        self.expires
            .checked_sub(now)
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "day request deadline passed"))
    }
}

impl<S: Timeouts + io::Read> io::Read for Deadline<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(self.remaining()?)?;
        self.stream.read(buf)
    }
}

impl<S: Timeouts + io::Write> io::Write for Deadline<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(self.remaining()?)?;
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn read_request(stream: &mut impl io::Read) -> Result<Request, Error> {
    let mut bytes = Vec::new();
    io::BufReader::new(stream.take(LIMIT + 1)).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > LIMIT || bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("request must be a bounded JSON line").into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_reply(stream: &mut impl io::Write, reply: &Reply) -> Result<(), Error> {
    serde_json::to_writer(&mut *stream, reply)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn read_reply(stream: &mut impl io::Read) -> Result<Reply, Error> {
    let mut bytes = Vec::new();
    io::BufReader::new(stream.take(1 << 20)).read_until(b'\n', &mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Process an already-authenticated request, recording identity from the kernel, not the request. This is the
/// service's whole answer to a peer `uid`, without the transport.
///
/// # Errors
/// A failed append or reapplication, which ends observation; refusals are [`Reply::Refused`].
pub fn respond(
    journal: &mut Journal,
    clock: &dyn Clock,
    uid: u32,
    request: Request,
    control: Controller<'_>,
) -> Result<Reply, super::Error> {
    match request {
        Request::Questions => Ok(Reply::Questions {
            pending: DayQuestions::of(journal.events())
                .pending()
                .cloned()
                .collect(),
            morning: crate::morning::Report::of(journal.events()),
        }),
        Request::State => Ok(Reply::State {
            status: control(journal, clock, Control::Status)?,
        }),
        Request::Pause(PauseRequest { state }) => {
            let refusal = match &state {
                PauseState::UntilBoot {} => None,
                PauseState::Until { deadline } if *deadline > clock.now() => None,
                PauseState::Until { .. } => Some("pause deadline must be in the future"),
                PauseState::Running {} => Some("use resume to end a pause"),
            };
            if let Some(reason) = refusal {
                return Ok(Reply::Refused {
                    reason: reason.into(),
                });
            }
            controlled(control(journal, clock, Control::Pause { state, uid }))
        }
        Request::Extend(ExtendRequest { revision }) => {
            controlled(control(journal, clock, Control::Extend { revision, uid }))
        }
        Request::Resume => controlled(control(journal, clock, Control::Resume { uid })),
        Request::Freeze(FreezeRequest { frozen }) => {
            controlled(control(journal, clock, Control::Freeze { frozen, uid }))
        }
        Request::Answer { boot, answer } => {
            let questions = DayQuestions::of(journal.events());
            let Some(end) = questions.get(&boot) else {
                return Ok(Reply::Refused {
                    reason: "no pending end exists for that boot".into(),
                });
            };
            let offsets = end.offsets;
            super::append(
                journal,
                clock,
                DayAnswer {
                    boot,
                    offsets,
                    answer,
                    uid,
                },
            )?;
            Ok(Reply::Answered)
        }
        Request::Reapply => {
            // Reapply is reserved for the system sleep hook, unlike user-session controls.
            if uid != 0 {
                return Ok(Reply::Refused {
                    reason: "reapplying offsets needs root".into(),
                });
            }
            control(journal, clock, Control::Reapply)?;
            Ok(Reply::Reapplied)
        }
        Request::Record { session, record } => {
            // Answering is open to the day group; recording a crash needs root, as the command does.
            if uid != 0 {
                return Ok(Reply::Refused {
                    reason: "recording a crash needs root".into(),
                });
            }
            // The command drafted against the state directory it was given, which need not be the service's.
            let live = journal.session();
            if live != Some(session) {
                return Ok(Reply::Refused {
                    reason: live.map_or_else(
                        || format!("the day service journal has no session, not session {session}"),
                        |live| {
                            format!("the day service records session {live}, not session {session}")
                        },
                    ),
                });
            }
            if let Err(refusal) = super::record::check(journal.events(), &record) {
                return Ok(Reply::Refused {
                    reason: togi_render::error_chain(&refusal),
                });
            }
            let seq = append_record(journal, clock, record).map_err(|error| match error {
                AppendRecordError::Append(source) => super::Error::Append(source),
                AppendRecordError::Clock(source) => super::Error::Observe(source),
            })?;
            Ok(Reply::Recorded { seq })
        }
    }
}

/// A control the current state no longer allows, such as a deadline that passed while the service checkpointed, is
/// the peer's refusal. Only a failed durable step ends the service.
fn controlled(result: Result<ControlStatus, super::Error>) -> Result<Reply, super::Error> {
    match result {
        Ok(status) => Ok(Reply::Controlled { status }),
        Err(
            refusal @ (super::Error::PauseDeadline
            | super::Error::PauseChanged
            | super::Error::PauseNotTimed),
        ) => Ok(Reply::Refused {
            reason: togi_render::error_chain(&refusal),
        }),
        Err(error) => Err(error),
    }
}

/// Sends one request to the running day service. A missing or unlistened socket is [`Error::Absent`]; every
/// other failure, refusals included, is the service's answer and never means no service runs.
pub fn request(request: &Request) -> Result<Reply, Error> {
    request_at(Path::new(PATH), request)
}

/// Only a socket path that does not exist or that nothing listens at means no service runs; the command then
/// writes the journal itself, so every other connect failure stays an error.
fn connect_error(error: io::Error) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        Error::Absent(error)
    } else {
        Error::Io(error)
    }
}

fn request_at(path: &Path, request: &Request) -> Result<Reply, Error> {
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    let mut stream = UnixStream::connect(path).map_err(connect_error)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Clients must not send answers to an endpoint impersonated by another user.
    if credentials(&stream)?.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "day service peer is not root",
        )
        .into());
    }
    if line.len() as u64 > LIMIT {
        return Err(Error::TooLarge { limit: LIMIT });
    }
    stream.write_all(&line)?;
    let reply = read_reply(&mut stream)?;
    match reply {
        Reply::Refused { reason } => Err(Error::Refused(reason)),
        Reply::Questions { .. }
        | Reply::State { .. }
        | Reply::Controlled { .. }
        | Reply::Answered
        | Reply::Recorded { .. }
        | Reply::Reapplied => Ok(reply),
    }
}

#[cfg(test)]
mod tests;
