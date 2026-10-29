//! Local, credential-checked transport for day questions and crash records. Only the observer owns the journal
//! writer.

use std::io::{self, BufRead as _, Read as _, Write as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use togi_journal::{
    DayAnswer, DayAnswerKind, DayQuestions, DayRecord, DayUncleanEnd, Journal, Seq,
};
use togi_machine::{BootId, Clock};
use togi_session::{AppendRecordError, append_record};

/// What the observer does for a root `Reapply` request. `Ok` means the whole transaction succeeded and is the
/// only thing that is acknowledged.
pub type Reapply<'a> = &'a mut dyn FnMut(&mut Journal, &dyn Clock) -> Result<(), super::Error>;

pub const PATH: &str = "/run/togi/day.sock";
/// Room for a crash record of every core with a long note; the deadline, not this, bounds a slow peer.
const LIMIT: u64 = 16 * 1024;
/// Total time one client may hold the observer loop, matching its one-second tick.
const EXCHANGE: Duration = Duration::from_secs(1);

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Questions,
    Answer {
        boot: BootId,
        answer: DayAnswerKind,
    },
    /// A crash record the user confirmed; the service checks it against the live journal before appending.
    Record {
        record: DayRecord,
    },
    /// Root only: reapply the day profile after resume; answered only once the transaction succeeded.
    Reapply,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Questions {
        pending: Vec<DayUncleanEnd>,
        morning: Option<crate::morning::Report>,
    },
    Answered,
    Recorded { seq: Seq },
    Reapplied,
    Refused { reason: String },
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
        reapply: Reapply<'_>,
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
                Ok(request) => respond(journal, clock, uid, request, reapply)?,
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

/// Process an already-authenticated request, recording identity from the kernel, not the request.
fn respond(
    journal: &mut Journal,
    clock: &dyn Clock,
    uid: u32,
    request: Request,
    reapply: Reapply<'_>,
) -> Result<Reply, super::Error> {
    let questions = DayQuestions::of(journal.events());
    match request {
        Request::Questions => Ok(Reply::Questions {
            pending: questions.pending().cloned().collect(),
            morning: crate::morning::Report::of(journal.events()),
        }),
        Request::Answer { boot, answer } => {
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
            // Even a member of the answer group may not move hardware.
            if uid != 0 {
                return Ok(Reply::Refused {
                    reason: "reapplying offsets needs root".into(),
                });
            }
            reapply(journal, clock)?;
            Ok(Reply::Reapplied)
        }
        Request::Record { record } => {
            // Answering is open to the day group; recording a crash needs root, as the command does.
            if uid != 0 {
                return Ok(Reply::Refused {
                    reason: "recording a crash needs root".into(),
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

/// Sends one request to the running day service. A missing or unlistened socket is [`Error::Absent`]; every
/// other failure, refusals included, is the service's answer and never means no service runs.
pub fn request(request: &Request) -> Result<Reply, Error> {
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    let mut stream = UnixStream::connect(PATH).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ) {
            Error::Absent(error)
        } else {
            Error::Io(error)
        }
    })?;
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
        Reply::Questions { .. } | Reply::Answered | Reply::Recorded { .. } | Reply::Reapplied => {
            Ok(reply)
        }
    }
}

#[cfg(test)]
mod tests;
