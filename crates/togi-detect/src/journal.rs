use std::ffi::OsString;
use std::num::ParseIntError;
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::value::RawValue;
use togi_command::Output;
use togi_machine::KernelCursor;
use togi_machine::serde_object::Object;

use crate::boot::JournalBoot;
use crate::error::{Error, Operation};
use crate::parse::Message;
use crate::reset::Position;

const NO_BOOT_ENTRY: &str = "No journal boot entry found";
const SEEK_FAILURE: &str = "Failed to seek to cursor";

fn argv(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

fn kernel_log(boot: &JournalBoot, fields: &str) -> Vec<OsString> {
    argv(&["-k", "-b", boot.as_str(), "-o", "json", fields])
}

pub(crate) fn mces_args(boot: &JournalBoot) -> Vec<OsString> {
    let mut args = kernel_log(
        boot,
        "--output-fields=MESSAGE,__REALTIME_TIMESTAMP,__MONOTONIC_TIMESTAMP",
    );
    args.extend(argv(&["--grep", "Hardware Error", "--no-pager", "-q"]));
    args
}

pub(crate) fn interval_args(boot: &JournalBoot, after: Option<&KernelCursor>) -> Vec<OsString> {
    let mut args = argv(&[
        "-b",
        boot.as_str(),
        "-o",
        "json",
        "--output-fields=MESSAGE,__REALTIME_TIMESTAMP,__MONOTONIC_TIMESTAMP,__CURSOR,_BOOT_ID,COREDUMP_PID,COREDUMP_COMM,COREDUMP_SIGNAL,COREDUMP_CGROUP",
        "--no-pager",
        "-q",
        "_TRANSPORT=kernel",
        "+",
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
    ]);
    if let Some(cursor) = after {
        args.push(format!("--cursor={cursor}").into());
    }
    args
}

pub(crate) fn reset_args(boot: &JournalBoot) -> Vec<OsString> {
    let mut args = kernel_log(boot, "--output-fields=MESSAGE");
    args.extend(argv(&[
        "--grep",
        "Linux version|Previous system reset reason",
        "--no-pager",
        "-q",
    ]));
    args
}

pub(crate) fn boots_args() -> Vec<OsString> {
    argv(&["--list-boots", "-o", "json", "--no-pager", "-q"])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Edge {
    Last,
    First,
}

pub(crate) fn boundary_args(boot: &JournalBoot, edge: Edge) -> Vec<OsString> {
    let count = match edge {
        Edge::Last => "1",
        Edge::First => "+1",
    };
    let mut args = argv(&[
        "-b",
        boot.as_str(),
        "-n",
        count,
        "-o",
        "json",
        "--output-fields=_BOOT_ID,__SEQNUM,__SEQNUM_ID",
        "--no-pager",
        "-q",
    ]);
    if edge == Edge::Last {
        args.push("-r".into());
    }
    args
}

/// What a command that exited with status 1 and wrote nothing means for a query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmptyFailure {
    NoMatch,
    BootMissing,
    Failure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExitRules {
    pub(crate) empty_failure: EmptyFailure,
    pub(crate) cursor_seek: bool,
}

impl ExitRules {
    pub(crate) const GREP: Self = Self {
        empty_failure: EmptyFailure::NoMatch,
        cursor_seek: false,
    };
    pub(crate) const fn cursor_read(resumed: bool) -> Self {
        Self {
            empty_failure: EmptyFailure::Failure,
            cursor_seek: resumed,
        }
    }
    pub(crate) const BOOT_MISSING_WHEN_EMPTY: Self = Self {
        empty_failure: EmptyFailure::BootMissing,
        cursor_seek: false,
    };
    pub(crate) const PLAIN: Self = Self {
        empty_failure: EmptyFailure::Failure,
        cursor_seek: false,
    };
}

/// The one place journalctl's exit status and stderr become a meaning. A query that matched nothing
/// answers with no rows.
pub(crate) fn classify(
    operation: &Operation,
    rules: ExitRules,
    output: Output,
) -> Result<Vec<u8>, Error> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(1) {
        if stderr.contains(NO_BOOT_ENTRY) {
            return Err(Error::BootMissing);
        }
        if output.stdout.is_empty() && stderr.is_empty() {
            match rules.empty_failure {
                EmptyFailure::NoMatch => return Ok(output.stdout),
                EmptyFailure::BootMissing => return Err(Error::BootMissing),
                EmptyFailure::Failure => {}
            }
        }
    }
    if rules.cursor_seek && stderr.contains(SEEK_FAILURE) {
        return Err(Error::CursorMissing);
    }
    Err(Error::Exit {
        operation: operation.clone(),
        status: output.status,
        stderr: stderr.trim().to_owned(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("the entry has no {0}")]
    Missing(&'static str),
    #[error("{field} {text:?} is not an integer in range")]
    Number {
        field: &'static str,
        text: String,
        #[source]
        source: ParseIntError,
    },
    #[error("{field} {text:?} is outside the range of timestamps")]
    Range {
        field: &'static str,
        text: String,
        #[source]
        source: jiff::Error,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Text {
    Plain(String),
    Bytes(Vec<u8>),
}

impl Text {
    fn into_string(self) -> String {
        match self {
            Self::Plain(text) => text,
            Self::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        }
    }
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "MESSAGE")]
    message: Option<Box<RawValue>>,
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    realtime: Option<String>,
    #[serde(rename = "__MONOTONIC_TIMESTAMP")]
    monotonic: Option<String>,
    #[serde(rename = "__CURSOR")]
    cursor: Option<String>,
    #[serde(rename = "_BOOT_ID")]
    boot: Option<String>,
    #[serde(rename = "COREDUMP_PID")]
    fault_pid: Option<String>,
    #[serde(rename = "COREDUMP_COMM")]
    fault_name: Option<String>,
    #[serde(rename = "COREDUMP_SIGNAL")]
    fault_signal: Option<String>,
    #[serde(rename = "COREDUMP_CGROUP")]
    fault_scope: Option<String>,
}

fn text_of(raw: Option<&RawValue>) -> Result<String, DecodeError> {
    let raw = raw.ok_or(DecodeError::Missing("MESSAGE"))?;
    Ok(serde_json::from_str::<Text>(raw.get())?.into_string())
}

impl Entry {
    fn text(&self) -> Result<String, DecodeError> {
        text_of(self.message.as_deref())
    }

    fn message(&self) -> Result<Message, DecodeError> {
        let text = self.text()?;
        let realtime = self
            .realtime
            .as_deref()
            .ok_or(DecodeError::Missing("__REALTIME_TIMESTAMP"))?;
        let monotonic = self
            .monotonic
            .as_deref()
            .ok_or(DecodeError::Missing("__MONOTONIC_TIMESTAMP"))?;
        let micros: i64 = realtime.parse().map_err(|source| DecodeError::Number {
            field: "__REALTIME_TIMESTAMP",
            text: realtime.to_owned(),
            source,
        })?;
        let time = Timestamp::from_microsecond(micros).map_err(|source| DecodeError::Range {
            field: "__REALTIME_TIMESTAMP",
            text: realtime.to_owned(),
            source,
        })?;
        let since_boot: u64 = monotonic.parse().map_err(|source| DecodeError::Number {
            field: "__MONOTONIC_TIMESTAMP",
            text: monotonic.to_owned(),
            source,
        })?;
        Ok(Message {
            time,
            text,
            monotonic: Duration::from_micros(since_boot),
        })
    }

    fn fault(&self, message: &Message) -> Result<Option<togi_machine::ProcessFault>, DecodeError> {
        let Some(signal) = self.fault_signal.as_deref() else {
            return Ok(None);
        };
        let signal = signal
            .parse::<u32>()
            .map_err(|source| DecodeError::Number {
                field: "COREDUMP_SIGNAL",
                text: signal.to_owned(),
                source,
            })?;
        let kind = match signal {
            11 => togi_machine::ProcessFaultKind::Segmentation,
            7 => togi_machine::ProcessFaultKind::Bus,
            4 => togi_machine::ProcessFaultKind::IllegalInstruction,
            6 => togi_machine::ProcessFaultKind::Abort,
            _ => return Ok(None),
        };
        let pid = self
            .fault_pid
            .as_deref()
            .ok_or(DecodeError::Missing("COREDUMP_PID"))?;
        let pid = pid.parse::<u32>().map_err(|source| DecodeError::Number {
            field: "COREDUMP_PID",
            text: pid.to_owned(),
            source,
        })?;
        let name = self
            .fault_name
            .clone()
            .ok_or(DecodeError::Missing("COREDUMP_COMM"))?;
        Ok(Some(togi_machine::ProcessFault {
            pid,
            name,
            kind,
            scope: self.fault_scope.clone(),
            cpu: None,
            core: None,
            monotonic: message.monotonic,
            lines: vec![message.text.clone()],
        }))
    }
}

/// Decodes one JSON object. A derived `Deserialize` also takes a positional array of the fields,
/// which journalctl never writes and a row must not be read from.
fn object<'a, T: Deserialize<'a>>(json: &'a [u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice::<Object<T>>(json).map(|Object(value)| value)
}

fn rows(out: &[u8]) -> impl Iterator<Item = &[u8]> {
    out.split(|&byte| byte == b'\n')
        .filter(|row| !row.is_empty())
}

pub(crate) fn mce_messages(out: &[u8], since: Duration) -> (Vec<Message>, Option<DecodeError>) {
    let mut messages = Vec::new();
    for row in rows(out) {
        let message = match object::<Entry>(row)
            .map_err(DecodeError::from)
            .and_then(|entry| entry.message())
        {
            Ok(message) => message,
            Err(error) => return (messages, Some(error)),
        };
        if message.monotonic >= since {
            messages.push(message);
        }
    }
    (messages, None)
}

pub(crate) fn reset_texts(out: &[u8]) -> Result<Vec<String>, DecodeError> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "MESSAGE")]
        message: Option<Box<RawValue>>,
    }
    rows(out)
        .map(|row| text_of(object::<Row>(row)?.message.as_deref()))
        .collect()
}

/// The kernel and coredump messages of a cursor read, and where the read ended.
pub(crate) struct Interval {
    pub(crate) messages: Vec<Message>,
    pub(crate) faults: Vec<togi_machine::ProcessFault>,
    pub(crate) end: Result<KernelCursor, IntervalError>,
}

pub(crate) enum IntervalError {
    CursorMissing,
    Decode(DecodeError),
}

/// Checks that every row belongs to `boot`, that the first row is the cursor read after, and that
/// each row can be decoded. Only the rows validated before a failure are returned with it.
pub(crate) fn read_interval(
    out: &[u8],
    boot: &JournalBoot,
    after: Option<&KernelCursor>,
) -> Interval {
    let mut messages = Vec::new();
    let mut faults = Vec::new();
    let mut last: Option<String> = None;
    for row in rows(out) {
        let entry = match object::<Entry>(row) {
            Ok(entry) => entry,
            Err(error) => {
                return Interval::failed(messages, faults, IntervalError::Decode(error.into()));
            }
        };
        let cursor = match (entry.cursor.as_deref(), entry.boot.as_deref()) {
            (Some(cursor), Some(row_boot)) if !cursor.is_empty() && row_boot == boot.as_str() => {
                cursor
            }
            _ => return Interval::failed(messages, faults, IntervalError::CursorMissing),
        };
        if let (Some(anchor), None) = (after, &last) {
            if cursor != anchor.as_str() {
                return Interval::failed(messages, faults, IntervalError::CursorMissing);
            }
            last = Some(cursor.to_owned());
            continue;
        }
        last = Some(cursor.to_owned());
        match entry.message() {
            Ok(message) => match entry.fault(&message) {
                Ok(Some(fault)) => faults.push(fault),
                Ok(None) => messages.push(message),
                Err(error) => {
                    return Interval::failed(messages, faults, IntervalError::Decode(error));
                }
            },
            Err(error) => return Interval::failed(messages, faults, IntervalError::Decode(error)),
        }
    }
    let end = last
        .map(KernelCursor::from)
        .ok_or(IntervalError::CursorMissing);
    Interval {
        messages,
        faults,
        end,
    }
}

impl Interval {
    fn failed(
        messages: Vec<Message>,
        faults: Vec<togi_machine::ProcessFault>,
        error: IntervalError,
    ) -> Self {
        Self {
            messages,
            faults,
            end: Err(error),
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct BootEntry {
    pub(crate) boot_id: String,
    #[serde(default)]
    pub(crate) first_entry: Option<i64>,
}

pub(crate) fn boot_entries(out: &[u8]) -> Result<Vec<BootEntry>, serde_json::Error> {
    let entries: Vec<Object<BootEntry>> = serde_json::from_slice(out)?;
    Ok(entries.into_iter().map(|Object(entry)| entry).collect())
}

#[derive(Deserialize)]
struct PositionRow {
    #[serde(rename = "_BOOT_ID")]
    boot: Option<String>,
    #[serde(rename = "__SEQNUM_ID")]
    sequence_id: Option<String>,
    #[serde(rename = "__SEQNUM")]
    sequence: Option<String>,
}

/// `None` when the output does not prove a position in `boot`'s part of the journal.
pub(crate) fn position(out: &[u8], boot: &JournalBoot) -> Result<Option<Position>, DecodeError> {
    if out.trim_ascii().is_empty() {
        return Ok(None);
    }
    let row: PositionRow = object(out)?;
    let sequence = row
        .sequence
        .as_deref()
        .map(|text| {
            text.parse::<u64>().map_err(|source| DecodeError::Number {
                field: "__SEQNUM",
                text: text.to_owned(),
                source,
            })
        })
        .transpose()?;
    Ok(match (row.boot, row.sequence_id, sequence) {
        (Some(row_boot), Some(sequence_id), Some(sequence))
            if row_boot == boot.as_str() && !sequence_id.is_empty() =>
        {
            Position::new(sequence_id, sequence)
        }
        _ => None,
    })
}
