//! Reading the journal for the read-only commands: the build check, the read, and the notices that
//! go with them.

use std::io::ErrorKind;

use togi_journal::{FileError, Operation, ReadError, Recording, Verdict, recorded_build};
use togi_render::{Renderer, Style, escape_text};

use crate::cli::Context;
use crate::exit::Exit;
use crate::report::{chain, current_build, fail, refusal_message};

/// What a read-only command says of an event kind this build does not know; `None` when `error` is
/// anything else. The kind goes in raw: the command's `fail` escapes it in the spec's human form.
#[must_use]
pub fn unknown_kind(error: &FileError) -> Option<String> {
    match &error.source {
        ReadError::UnknownKind { line, source } => Some(format!(
            "line {line}: unknown event kind \"{}\"; this build does not know it",
            source.0
        )),
        _ => None,
    }
}

fn notice(ctx: &mut Context<'_>, style: Style, message: &str) {
    let renderer = Renderer::new(ctx.system.stderr_facts());
    let message = escape_text(message).to_string();
    let _ = writeln!(ctx.err, "{}", renderer.text(style, &message));
}

/// Reads the live journal of `ctx.state_dir`; on failure writes the reason to stderr and gives the
/// exit. A different ruleset warns, a different schema is refused, a torn tail is reported by
/// [`report_torn`] once the command has printed.
///
/// # Errors
///
/// The [`Exit`] the command returns: [`Exit::Error`] for a missing, unreadable or incompatible
/// journal.
pub fn open(ctx: &mut Context<'_>, command: &str) -> Result<Recording, Exit> {
    let current = current_build();
    let recorded = match recorded_build(ctx.state_dir) {
        Ok(recorded) => recorded,
        Err(error) => return Err(fail(ctx, command, chain(&error))),
    };
    if let Some(recorded) = &recorded {
        match current.verdict(recorded, Operation::Inspect) {
            Verdict::Refuse(refusal) => {
                let message = refusal_message(&refusal, recorded, &current);
                notice(ctx, Style::RedBold, &format!("togi {command}: {message}"));
                return Err(Exit::Error);
            }
            Verdict::Use | Verdict::Archive if recorded.ruleset != current.ruleset => {
                let message = format!(
                    "togi {command}: the journal's ruleset is {}, this build's is {}; what it shows may not follow this build's rules",
                    recorded.ruleset, current.ruleset
                );
                notice(ctx, Style::Yellow, &message);
            }
            Verdict::Use | Verdict::Archive => {}
        }
    }
    let path = ctx.state_dir.join("events.jsonl");
    Recording::read(&path).map_err(|error| {
        if matches!(&error.source, ReadError::Io(io) if io.kind() == ErrorKind::NotFound) {
            return fail(ctx, command, format!("no journal at {}", path.display()));
        }
        let message = unknown_kind(&error).unwrap_or_else(|| chain(&error));
        fail(ctx, command, message)
    })
}

/// Tells the operator the journal ends mid-line; the next `run` records it.
pub fn report_torn(ctx: &mut Context<'_>, command: &str, recording: &Recording) {
    if let Some(torn) = recording.torn() {
        let _ = writeln!(
            ctx.err,
            "togi {command}: journal ends with {} torn bytes; the next run records journal.torn",
            torn.bytes.len()
        );
    }
}
