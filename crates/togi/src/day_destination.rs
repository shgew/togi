//! Day observation's journal destination, deliberately separate from its lifecycle.
//!
//! Observation appends to an existing compatible session; it never creates a tuning session.

use togi_journal::{Build, Journal, Operation, Payload, Verdict};
use togi_machine::BootId;

use crate::cli::Context;
use crate::exit::Exit;
use crate::report::{exit_with, refusal_message};

/// Checks the destination without creating any state files.
pub fn available(ctx: &mut Context<'_>, build: &Build) -> Result<(), Exit> {
    let recorded = ctx
        .system
        .recorded_build(ctx.state_dir)
        .map_err(|error| exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error))?;
    compatible(ctx, build, recorded.as_ref())
}

fn compatible(ctx: &mut Context<'_>, build: &Build, recorded: Option<&Build>) -> Result<(), Exit> {
    let Some(recorded) = recorded else {
        return Err(exit_with(
            ctx,
            "day",
            "no session journal; start a session with togi run before enabling daily observation",
            Exit::Error,
        ));
    };
    match build.verdict(recorded, Operation::Append) {
        Verdict::Use => Ok(()),
        Verdict::Refuse(refusal) => Err(exit_with(
            ctx,
            "day",
            refusal_message(&refusal, recorded, build),
            Exit::Incompatible,
        )),
        Verdict::Archive => Err(exit_with(
            ctx,
            "day",
            "the session must be transitioned by togi run before daily observation",
            Exit::Incompatible,
        )),
    }
}

/// Opens the session under its existing writer lock and rechecks compatibility before appending.
pub fn open(ctx: &mut Context<'_>, boot: BootId, build: &Build) -> Result<Journal, Exit> {
    available(ctx, build)?;
    let dir = ctx.system.lock_state_dir(ctx.state_dir).map_err(|error| {
        let exit = if matches!(error, togi_journal::LockError::Held { .. }) {
            Exit::Locked
        } else {
            Exit::Error
        };
        exit_with(ctx, "day", togi_render::error_chain(&error), exit)
    })?;
    let recorded = dir
        .live_build()
        .map_err(|error| exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error))?;
    compatible(ctx, build, recorded.as_ref())?;
    let recording = ctx
        .system
        .recording(&ctx.state_dir.join("events.jsonl"))
        .map_err(|error| {
            let unknown = recorded
                .as_ref()
                .and_then(|recorded| crate::report::unknown_event_message(&error, recorded, build));
            match unknown {
                Some(message) => exit_with(ctx, "day", message, Exit::Incompatible),
                None => exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error),
            }
        })?;
    if recording
        .events()
        .iter()
        .any(|event| matches!(event.payload, Payload::SessionArchived(_)))
    {
        return Err(exit_with(
            ctx,
            "day",
            "the session is archived; start a new session with togi run",
            Exit::Error,
        ));
    }
    dir.open_at(boot, ctx.system.now())
        .map_err(|error| exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error))
}
