use togi_journal::{
    Certainty, Classification, Edge, Outcome, Payload, QualificationState, StepKind, VetoState,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Style {
    #[default]
    Plain,
    Red,
    RedBold,
    Green,
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
/// `mce`, the carried copies, every `tuner.decision`, `core.phase`, `checking.*`,
/// `deepening.round`, `hunt.*` and `combination` event, `host.ranking`, `defect.answered`,
/// `command.reset` and every session, boot and journal
/// event except `session.warning`. Ruleset 13 kinds are plain except a `ccd.edge` with an observed failure, a failed
/// qualification and a CPU-suspect `day.event` (red), a shallower `ccd.cap`, a retreat or firmware-zero
/// `day.step` and an active `day.veto` (yellow), and a complete qualification and promoting step (green).
/// A new kind lands here by naming its arm.
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
        Payload::DefectFound(_) | Payload::TunerWarning(_) | Payload::SessionWarning(_) => {
            Style::Yellow
        }
        Payload::BackendRetry(_) => Style::Dim,
        Payload::CcdEdge(edge) => match edge.edge {
            Edge::Observed { .. } => Style::Red,
            Edge::Unknown {} | Edge::Censored { .. } => Style::Plain,
        },
        Payload::CcdCap(cap) => {
            if cap.new > cap.old {
                Style::Yellow
            } else {
                Style::Plain
            }
        }
        Payload::NightQualification(qualification) => match qualification.state {
            QualificationState::Failed { .. } => Style::Red,
            QualificationState::Complete { .. } => Style::Green,
            QualificationState::Start { .. } | QualificationState::Progress { .. } => Style::Plain,
        },
        Payload::DayStep(step) => match step.step {
            StepKind::Promote { .. } => Style::Green,
            StepKind::Retreat { .. } | StepKind::FirmwareZero { .. } => Style::Yellow,
            StepKind::Hold { .. } => Style::Plain,
        },
        Payload::DayEvent(event) => match event.classification {
            Classification::CpuSuspect {
                certainty: Certainty::Definite | Certainty::Provisional,
            } => Style::Red,
            Classification::NonCpu {} | Classification::Unexplained {} => Style::Plain,
        },
        Payload::DayVeto(veto) => match veto.state {
            VetoState::Active => Style::Yellow,
            VetoState::Cleared => Style::Plain,
        },
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
        | Payload::TunerDecision(_)
        | Payload::CorePhase(_)
        | Payload::CheckingCycle(_)
        | Payload::DeepeningRound(_)
        | Payload::HuntStart(_)
        | Payload::HuntEnd(_)
        | Payload::Combination(_)
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
        | Payload::DayFault(_)
        | Payload::DayUncleanEnd(_)
        | Payload::SessionImported(_)
        | Payload::ExposureObserved(_)
        | Payload::NightPlan(_)
        | Payload::TaskStart(_)
        | Payload::NightReproduction(_)
        | Payload::DayApplied(_)
        | Payload::DayHold(_)
        | Payload::DayFreeze(_)
        | Payload::DayInterval(_)
        | Payload::DayPause(_)
        | Payload::DayImported(_)
        | Payload::StateRebuilt(_) => Style::Plain,
    }
}
