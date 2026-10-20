//! How a run's end reads on stderr and which status it exits with.

use std::io::Write;

use jiff::tz::TimeZone;
use togi_journal::{Event, EventKind, Seq};
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
        return Outcome::exit(match first {
            SessionError::Compatibility(_) => Exit::Incompatible,
            SessionError::NoSuchCore { .. } => Exit::Usage,
            SessionError::UnsupportedTopology(_) => Exit::Preflight,
            SessionError::Journal(_)
            | SessionError::Fold(_)
            | SessionError::Machine(_)
            | SessionError::Config(_)
            | SessionError::Bootloader(_)
            | SessionError::Lookup(_)
            | SessionError::Write(_)
            | SessionError::Log(_)
            | SessionError::TopologyChanged { .. }
            | SessionError::NoSuchTrialCore { .. }
            | SessionError::Unproven
            | SessionError::PlanDisagrees
            | SessionError::NotReady(_)
            | SessionError::ClearSavedEntry(_)
            | SessionError::SessionId(_)
            | SessionError::DeadEndUnrecorded(_)
            | SessionError::Prompt { .. }
            | SessionError::Panicked(_)
            | SessionError::TrialDuration(_)
            | SessionError::TrialTemperature(_)
            | SessionError::RecoveredElapsed(_)
            | SessionError::WorkloadRotation(_)
            | SessionError::Monotonic(_)
            | SessionError::Profile(_) => Exit::Error,
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

/// The closing events (`profile.restored`, `shutdown`) of the run's stop that the dashboard kept off the screen,
/// told event by event as they are appended.
///
/// A restoration belongs to the stop only if nothing but the closing sequence's own events follows it: a
/// same-boot resume restores at its startup and then goes on to work, and that restoration is not the stop's. The
/// closing sequence's own events are the `shutdown` and what its kernel-log read records first, warnings and
/// machine checks. A line the dashboard did not keep off the screen, because it had given way to event lines,
/// was printed then and is not tracked.
#[derive(Debug, Default)]
pub(super) struct HiddenClosing(Vec<Seq>);

impl HiddenClosing {
    /// Takes in an appended event, and whether the dashboard kept its line off the screen.
    pub(super) fn appended(&mut self, kind: EventKind, seq: Seq, hidden: bool) {
        match kind {
            EventKind::ProfileRestored => {
                self.0.clear();
                if hidden {
                    self.0.push(seq);
                }
            }
            EventKind::Shutdown => {
                if hidden {
                    self.0.push(seq);
                }
            }
            EventKind::SessionWarning | EventKind::Mce => {}
            EventKind::SessionStart
            | EventKind::SessionContext
            | EventKind::SessionBaseline
            | EventKind::SessionNotice
            | EventKind::SessionArchived
            | EventKind::SessionCarried
            | EventKind::ConfigLoaded
            | EventKind::PreflightCheck
            | EventKind::SmuIntent
            | EventKind::SmuWrite
            | EventKind::SmuReadback
            | EventKind::SmuError
            | EventKind::ProfileApplied
            | EventKind::ProfileChange
            | EventKind::TrialIntent
            | EventKind::TrialStart
            | EventKind::TrialProgress
            | EventKind::TrialSignal
            | EventKind::TrialSample
            | EventKind::TrialEnd
            | EventKind::TrialCarried
            | EventKind::Failure
            | EventKind::FailureCarried
            | EventKind::CrashDetected
            | EventKind::TunerDecision
            | EventKind::CorePhase
            | EventKind::CheckingCycle
            | EventKind::CheckingStep
            | EventKind::CheckingChain
            | EventKind::HostRanking
            | EventKind::HuntStart
            | EventKind::HuntGroup
            | EventKind::HuntEnd
            | EventKind::HuntSkipped
            | EventKind::Combination
            | EventKind::DeepeningRound
            | EventKind::TunerWarning
            | EventKind::BackendRetry
            | EventKind::CommandReset
            | EventKind::DefectFound
            | EventKind::DefectAnswered
            | EventKind::DeadEnd
            | EventKind::BootSavedEntry
            | EventKind::BootLeaveReason
            | EventKind::JournalTorn
            | EventKind::DayBootStart
            | EventKind::DayCheckpoint
            | EventKind::DayOffsetsChanged
            | EventKind::DayCleanStop
            | EventKind::DayUncleanEnd
            | EventKind::StateRebuilt => self.0.clear(),
        }
    }

    pub(super) fn seqs(&self) -> &[Seq] {
        &self.0
    }
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
