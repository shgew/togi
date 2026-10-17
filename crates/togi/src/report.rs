//! What commands that read or change a journal share: this build's stamp, the wording of a
//! refused journal, error chains and checked writes to stdout.

use std::fmt::{Display, Write as _};

use togi_defect::{KNOWN_DEFECTS, fixed_stamp};
use togi_journal::{Build, Dimension, Event, FileError, ReadError, Refusal, SCHEMA};
use togi_render::{Renderer, escape_text};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::cli::Context;
use crate::exit::Exit;

/// The build that stamps every session this binary starts.
#[must_use]
pub fn current_build() -> Build {
    Build {
        version: togi_version::version().to_owned(),
        rev: Some(togi_version::rev().to_owned()),
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: fixed_stamp(KNOWN_DEFECTS).0,
        evidence: EVIDENCE_EPOCH,
    }
}

/// Why `current` may not use a journal `recorded` wrote, with what to do about it.
#[must_use]
pub fn refusal_message(refusal: &Refusal, recorded: &Build, current: &Build) -> String {
    let Refusal {
        dimension,
        current: used,
        ..
    } = refusal;
    let mut written = format!(
        "this journal was written by {recorded} (schema {}, ruleset {}",
        recorded.schema, recorded.ruleset
    );
    if *dimension == Dimension::Evidence {
        let _ = write!(written, ", evidence epoch {}", recorded.evidence);
    }
    let newer = recorded.schema > current.schema
        || recorded.ruleset > current.ruleset
        || recorded.evidence > current.evidence;
    let mut text = format!("{written}); this build, {current}, uses {dimension} {used}. ");
    if newer {
        let _ = write!(
            text,
            "Install togi {} to continue this session, or run togi reset --all to archive it and start over.",
            recorded.version
        );
    } else {
        text.push_str(
            "togi run archives it and starts a new session that carries its candidate solo limits and failure points; togi reset --all archives it and starts over.",
        );
    }
    text
}

/// Writes `text` and a newline to stdout; a failed write is [`Exit::Error`].
pub fn emit(ctx: &mut Context<'_>, text: impl Display) -> Result<(), Exit> {
    writeln!(ctx.out, "{text}").map_err(|error| ctx.error(format!("write output: {error}")))
}

/// Writes `togi <command>: <message>` to stderr, the message escaped, and returns `exit`.
pub fn exit_with(ctx: &mut Context<'_>, command: &str, message: impl Display, exit: Exit) -> Exit {
    let _ = writeln!(
        ctx.err,
        "togi {command}: {}",
        escape_text(&message.to_string())
    );
    exit
}

/// [`exit_with`] for [`Exit::Error`].
pub fn fail(ctx: &mut Context<'_>, command: &str, message: impl Display) -> Exit {
    exit_with(ctx, command, message, Exit::Error)
}

/// What to tell of a journal that holds an event this build cannot decode strictly: a kind it does not know or a field
/// it does not know; `None` when `error` is anything else.
#[must_use]
pub fn unknown_event_message(
    error: &FileError,
    recorded: &Build,
    current: &Build,
) -> Option<String> {
    let unknown = match &error.source {
        ReadError::UnknownKind { source, .. } => format!("unknown kind {:?}", source.0),
        ReadError::UnknownField { kind, field, .. } => {
            format!("unknown field {field:?} of a {kind} event")
        }
        _ => return None,
    };
    Some(format!(
        "{unknown} in journal written by {recorded} (schema {}, ruleset {}); this build, {current} (schema {}, ruleset {}), cannot safely modify it; install the build that wrote it",
        recorded.schema, recorded.ruleset, current.schema, current.ruleset,
    ))
}

/// Writes the lines of `events` to stderr, in local time and without colour.
pub fn log_events(ctx: &mut Context<'_>, events: &[Event]) {
    let zone = ctx.system.zone();
    for event in events {
        let _ = Renderer::default().write_event(&mut *ctx.err, event, &zone);
    }
}
