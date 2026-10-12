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
/// [`Exit::Usage`], any other error [`Exit::Error`]; without errors a signal, the cycles or the first result
/// exit 0 and a dead end exits with its own status.
pub(super) fn conclude(
    err: &mut dyn Write,
    renderer: Renderer,
    zone: &TimeZone,
    stop: Option<&Stop>,
    errors: &[SessionError],
    events: &[Event],
) -> Outcome {
    if let Some(first) = errors.first() {
        for error in errors {
            let line = format!("togi run: {}", escape_text(&error.to_string()));
            let style = if matches!(error, SessionError::Compatibility(_)) {
                Style::RedBold
            } else {
                Style::Plain
            };
            let _ = writeln!(err, "{}", renderer.text(style, &line));
        }
        return Outcome::exit(match first {
            SessionError::Compatibility(_) => Exit::Incompatible,
            SessionError::NoSuchCore { .. } => Exit::Usage,
            _ => Exit::Error,
        });
    }
    match stop {
        Some(Stop::Signal | Stop::Cycles | Stop::FirstResult) => Outcome::exit(Exit::Ok),
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
    }
}

fn find(events: &[Event], seq: Seq) -> Option<&Event> {
    events.iter().find(|event| event.seq == seq)
}

/// The lines of the events in `hidden`, which the dashboard kept off the screen, in journal order.
pub(super) fn print_hidden(
    err: &mut dyn Write,
    renderer: Renderer,
    zone: &TimeZone,
    events: &[Event],
    hidden: &[Seq],
) {
    for event in events.iter().filter(|event| hidden.contains(&event.seq)) {
        let _ = writeln!(err, "{}", renderer.line(event, zone));
    }
}

#[cfg(test)]
mod tests;
