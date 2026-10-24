use togi_journal::{CorePhase, CycleEvent, Decision, HuntOutcome, Outcome, Payload, RoundEvent};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Style {
    #[default]
    Plain,
    Red,
    RedBold,
    Green,
    GreenBold,
    Yellow,
    Dim,
}

impl Style {
    pub(crate) const fn sgr(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Red => Some("\x1b[31m"),
            Self::RedBold => Some("\x1b[1;31m"),
            Self::Green => Some("\x1b[32m"),
            Self::GreenBold => Some("\x1b[1;32m"),
            Self::Yellow => Some("\x1b[33m"),
            Self::Dim => Some("\x1b[2m"),
        }
    }

    pub(crate) const fn is_error(self) -> bool {
        matches!(self, Self::Red | Self::RedBold)
    }
}

/// The colour of an event's human-readable line, per the table in `docs/spec/journal.md` (Colors).
///
/// Plain, deliberately: a passed `trial.end`, `smu.*`, `profile.*`, `trial.*` other than the end,
/// `mce`, the carried copies, `checking.step` and `checking.chain`, `host.ranking`, `hunt.group`
/// (even with an inferred failure), `hunt.skipped`, `hunt.end` result `cancelled`, cycle and round
/// starts, a failed or non-full cycle end, a failed deepening end,
/// core transitions other than reaching the limit or leaving search with room, the `check_solo_limit`
/// and `hold` decisions, `defect.answered`, `command.reset` and every session, boot and journal
/// event except `session.warning`. A new kind lands here by naming its arm.
#[must_use]
pub fn style_for(payload: &Payload) -> Style {
    match payload {
        Payload::TrialEnd(end) => match end.outcome {
            Outcome::Failure => Style::Red,
            Outcome::Inconclusive => Style::Dim,
            Outcome::Pass => Style::Plain,
        },
        Payload::Failure(_) | Payload::CrashDetected(_) => Style::Red,
        Payload::DeadEnd(_) => Style::RedBold,
        Payload::TunerDecision(decision) => match decision.decision {
            Decision::StepDeeper | Decision::Deepen => Style::Green,
            Decision::Backoff | Decision::Yield => Style::Yellow,
            Decision::CheckSoloLimit | Decision::Hold => Style::Plain,
        },
        Payload::HuntEnd(end) => match end.result {
            HuntOutcome::Culprit | HuntOutcome::Direct => Style::Green,
            HuntOutcome::Combination | HuntOutcome::Fallback | HuntOutcome::Loaded => Style::Yellow,
            HuntOutcome::Cancelled => Style::Plain,
        },
        Payload::CorePhase(transition) => {
            let reaches_limit = transition.to == CorePhase::AtLimit;
            let leaves_search_with_room =
                transition.to == CorePhase::HasRoom && transition.from == Some(CorePhase::Search);
            if reaches_limit || leaves_search_with_room {
                Style::GreenBold
            } else {
                Style::Plain
            }
        }
        Payload::CheckingCycle(cycle) => {
            if cycle.event == CycleEvent::End && cycle.passed && cycle.full {
                Style::GreenBold
            } else {
                Style::Plain
            }
        }
        Payload::DeepeningRound(round) => {
            if round.event == RoundEvent::End && round.passed {
                Style::GreenBold
            } else {
                Style::Plain
            }
        }
        Payload::DefectFound(_)
        | Payload::HuntStart(_)
        | Payload::Combination(_)
        | Payload::TunerWarning(_)
        | Payload::SessionWarning(_) => Style::Yellow,
        Payload::BackendRetry(_) => Style::Dim,
        Payload::SessionStart(_)
        | Payload::SessionContext(_)
        | Payload::SessionBaseline(_)
        | Payload::SessionNotice(_)
        | Payload::SessionArchived(_)
        | Payload::SessionCarried(_)
        | Payload::ConfigLoaded(_)
        | Payload::PreflightCheck(_)
        | Payload::SmuIntent(_)
        | Payload::SmuWrite(_)
        | Payload::SmuReadback(_)
        | Payload::SmuError(_)
        | Payload::ProfileApplied(_)
        | Payload::ProfileChange(_)
        | Payload::ProfileRestored(_)
        | Payload::TrialIntent(_)
        | Payload::TrialStart(_)
        | Payload::TrialProgress(_)
        | Payload::TrialSignal(_)
        | Payload::TrialSample(_)
        | Payload::TrialCarried(_)
        | Payload::FailureCarried(_)
        | Payload::Mce(_)
        | Payload::CheckingStep(_)
        | Payload::CheckingChain(_)
        | Payload::HostRanking(_)
        | Payload::HuntGroup(_)
        | Payload::HuntSkipped(_)
        | Payload::CommandReset(_)
        | Payload::DefectAnswered(_)
        | Payload::BootSavedEntry(_)
        | Payload::BootLeaveReason(_)
        | Payload::Shutdown(_)
        | Payload::JournalTorn(_)
        | Payload::DayBootStart(_)
        | Payload::DayCheckpoint(_)
        | Payload::DayOffsetsChanged(_)
        | Payload::DayCleanStop(_)
        | Payload::DayAnswer(_)
        | Payload::DayRecord(_)
        | Payload::DayUncleanEnd(_)
        | Payload::StateRebuilt(_) => Style::Plain,
    }
}
