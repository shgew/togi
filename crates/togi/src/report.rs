//! What commands that read or change a journal share: this build's stamp, the wording of a
//! refused journal, error chains and checked writes to stdout.

use std::error::Error;
use std::fmt::{Display, Write as _};

use togi_defect::{KNOWN_DEFECTS, fixed_stamp};
use togi_journal::{Build, Refusal, SCHEMA};
use togi_render::escape_text;
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
        recorded: found,
        current: used,
    } = refusal;
    let mut text = format!(
        "this journal was written by {recorded} ({dimension} {found}); this build, {current}, uses {dimension} {used}. "
    );
    if found > used {
        let name = recorded.to_string();
        let _ = write!(
            text,
            "Install {name} to continue this session, or run togi reset --all to archive it and start over."
        );
    } else {
        text.push_str(
            "togi run archives it and starts a new session that carries its candidate solo limits and failure points; togi reset --all archives it and starts over.",
        );
    }
    text
}

/// `error` and every cause below it, joined by `: `.
#[must_use]
pub fn chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = write!(text, ": {cause}");
        source = cause.source();
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
