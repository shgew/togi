//! How a run's end reads on stderr and which status it exits with.

use std::io::Write;

use jiff::tz::TimeZone;
use togi_journal::{Event, Seq};
use togi_render::{Renderer, Style, escape_text};
use togi_session::{SessionError, Stop};

use crate::exit::Exit;

/// What the run's end asks of the caller after the diagnostics are written.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Outcome {
    pub(super) exit: Exit,
    /// A dead end asked for a reboot into the normal system.
    pub(super) reboot: bool,
}

impl Outcome {
    const fn exit(exit: Exit) -> Self {
        Self {
            exit,
            reboot: false,
        }
    }
}

/// Writes the diagnostics of a run that ended with `stop` and `errors`, citing `events` for a dead end's
/// evidence, and says how the process exits. A refused journal is [`Exit::Incompatible`], a missing core
/// [`Exit::Usage`], a topology the tuner cannot work on [`Exit::Preflight`], any other error
/// [`Exit::Error`]; without errors a signal, the cycles or the first result exit 0 and a dead end exits
/// with its own status.
/// Errors are printed first and determine the outcome without suppressing dead-end diagnostics.
pub(super) fn conclude(
    err: &mut dyn Write,
    renderer: Renderer,
    zone: &TimeZone,
    stop: Option<&Stop>,
    errors: &[SessionError],
    events: &[Event],
) -> Outcome {
    let failure = errors.first().map(|first| {
        for error in errors {
            let line = format!(
                "togi run: {}",
                escape_text(&togi_render::error_chain(error))
            );
            let style = if matches!(error, SessionError::Compatibility(_)) {
                Style::RedBold
            } else {
                Style::Plain
            };
            let _ = writeln!(err, "{}", renderer.text(style, &line));
        }
        Outcome::exit(match first {
            SessionError::Compatibility(_) => Exit::Incompatible,
            SessionError::UnsupportedTopology(_)
            | SessionError::BaselineNotZero { .. }
            | SessionError::BaselineIncomplete { .. } => Exit::Preflight,
            SessionError::Journal(_)
            | SessionError::Fold(_)
            | SessionError::Machine(_)
            | SessionError::Config(_)
            | SessionError::Bootloader(_)
            | SessionError::Lookup(_)
            | SessionError::Write(_)
            | SessionError::Log(_)
            | SessionError::TopologyChanged { .. }
            | SessionError::Unproven
            | SessionError::Plan(_)
            | SessionError::NotReady(_)
            | SessionError::ClearSavedEntry(_)
            | SessionError::SessionId(_)
            | SessionError::DeadEndUnrecorded(_)
            | SessionError::Panicked(_)
            | SessionError::TrialDuration(_)
            | SessionError::TrialTemperature(_)
            | SessionError::RecoveredElapsed(_)
            | SessionError::Monotonic(_)
            | SessionError::Profile(_) => Exit::Error,
        })
    });
    let stopped = match stop {
        Some(Stop::Signal | Stop::Night(_)) => Outcome::exit(Exit::Ok),
        Some(Stop::DeadEnd {
            dead_end,
            causes,
            reboot,
        }) => {
            let line = format!(
                "togi: dead end {}: {}",
                dead_end.condition.as_str(),
                dead_end.detail
            );
            let _ = writeln!(err, "{}", renderer.text(Style::RedBold, &line));
            for seq in causes {
                if let Some(event) = find(events, *seq) {
                    let _ = writeln!(
                        err,
                        "{}",
                        renderer.prefixed_line(event, zone, "  evidence: ")
                    );
                }
            }
            Outcome {
                exit: Exit::dead_end(dead_end.condition),
                reboot: *reboot,
            }
        }
        None => Outcome::exit(Exit::Error),
    };
    failure.unwrap_or(stopped)
}

/// Reports uncertain offsets independently of the stop and any cleanup errors.
pub(super) fn stock_not_verified(err: &mut dyn Write, renderer: Renderer) {
    let _ = writeln!(
        err,
        "{}",
        renderer.text(
            Style::RedBold,
            "togi: stock not verified; current offsets unknown"
        )
    );
}

fn find(events: &[Event], seq: Seq) -> Option<&Event> {
    events.iter().find(|event| event.seq == seq)
}

#[cfg(test)]
mod tests;
