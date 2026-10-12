//! Reading the journal for the read-only commands: the build check, the read, and the notices that
//! go with them.

use std::io::{ErrorKind, stderr};

use togi_journal::{
    Build, FileError, Operation, ReadError, Recording, SCHEMA, Verdict, recorded_build,
};
use togi_render::{OutputFacts, Renderer, Style, escape_text};

use crate::cli::Context;
use crate::exit::Exit;

/// The build this binary records and judges journals by.
#[must_use]
pub fn current_build() -> Build {
    let rev = togi_version::rev();
    Build {
        version: togi_version::version().to_owned(),
        rev: (!rev.is_empty()).then(|| rev.to_owned()),
        ruleset: togi_tuner::RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: togi_tuner::EVIDENCE_EPOCH,
    }
}

fn stderr_renderer() -> Renderer {
    Renderer::new(OutputFacts::detect(
        &stderr(),
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("JOURNAL_STREAM").as_deref(),
    ))
}

fn notice(ctx: &mut Context<'_>, style: Style, message: &str) {
    let renderer = stderr_renderer();
    let _ = writeln!(ctx.err, "{}", renderer.text(style, message));
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
    match recorded_build(ctx.state_dir) {
        Ok(Some(recorded)) => match current.verdict(&recorded, Operation::Inspect) {
            Verdict::Refuse(refusal) => {
                let advice = if refusal.recorded > refusal.current {
                    format!("install {recorded} to read it, or run `togi reset --all`")
                } else {
                    "run `togi reset --all` to start a new session".to_owned()
                };
                notice(
                    ctx,
                    Style::RedBold,
                    &format!(
                        "togi: {command}: the journal was written by {recorded}: {refusal}; {advice}"
                    ),
                );
                return Err(Exit::Error);
            }
            Verdict::Use | Verdict::Archive => {
                if recorded.ruleset != current.ruleset {
                    notice(
                        ctx,
                        Style::Yellow,
                        &format!(
                            "togi: {command}: the journal's ruleset is {}, this build's is {}; what it shows may not follow this build's rules",
                            recorded.ruleset, current.ruleset
                        ),
                    );
                }
            }
        },
        Ok(None) => {}
        Err(error) => {
            return Err(ctx.error(format_args!(
                "{command}: {}",
                escape_text(&error.to_string())
            )));
        }
    }
    let path = ctx.state_dir.join("events.jsonl");
    Recording::read(&path).map_err(|error| match &error {
        FileError {
            source: ReadError::Io(io),
            ..
        } if io.kind() == ErrorKind::NotFound => {
            ctx.error(format_args!("{command}: no journal at {}", path.display()))
        }
        _ => ctx.error(format_args!(
            "{command}: {}",
            escape_text(&error.to_string())
        )),
    })
}

/// Tells the operator the journal ends mid-line; the next `run` records it.
pub fn report_torn(ctx: &mut Context<'_>, command: &str, recording: &Recording) {
    if let Some(torn) = recording.torn() {
        let _ = writeln!(
            ctx.err,
            "togi: {command}: journal ends with {} torn bytes; the next run records journal.torn",
            torn.bytes.len()
        );
    }
}
