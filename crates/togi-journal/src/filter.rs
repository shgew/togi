use std::collections::HashSet;
use std::str::FromStr;

use jiff::Timestamp;
use togi_machine::CoreSet;

use crate::payload::{EventKind, Payload, TrialId};
use crate::{Event, Seq};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidKindSelector {
    #[error("empty kind list; valid names: {}", KindSelector::names().join(", "))]
    Empty,
    #[error("unknown kind or group {name:?}; valid names: {}", KindSelector::names().join(", "))]
    Unknown { name: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KindSelector {
    Kind(EventKind),
    Group(&'static str),
}

impl KindSelector {
    pub fn names() -> Vec<&'static str> {
        let mut names: Vec<_> = EventKind::ALL
            .iter()
            .flat_map(|kind| [kind.as_str(), kind.group()])
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    pub fn matches(self, kind: EventKind) -> bool {
        match self {
            Self::Kind(exact) => exact == kind,
            Self::Group(group) => kind.group() == group,
        }
    }
}

impl FromStr for KindSelector {
    type Err = InvalidKindSelector;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if name.is_empty() {
            return Err(InvalidKindSelector::Empty);
        }
        let unknown = || InvalidKindSelector::Unknown {
            name: name.to_owned(),
        };
        if name.contains('.') {
            return name.parse().map(Self::Kind).map_err(|_| unknown());
        }
        EventKind::ALL
            .iter()
            .find(|kind| kind.group() == name)
            .map(|kind| Self::Group(kind.group()))
            .ok_or_else(unknown)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Filter {
    pub cores: CoreSet,
    pub kinds: Vec<KindSelector>,
    pub trial: Option<TrialId>,
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
}

#[derive(Default)]
struct Widening {
    loading: HashSet<TrialId>,
    matched: HashSet<Seq>,
}

impl Filter {
    pub fn matches(&self, event: &Event) -> bool {
        self.evaluate(event, None)
    }

    pub fn select<'a>(&self, events: &'a [Event]) -> Vec<&'a Event> {
        let mut widening = (!self.cores.is_empty()).then(Widening::default);
        let mut selected = Vec::new();
        for event in events {
            if let (Some(widening), Payload::TrialIntent(intent)) = (&mut widening, &event.payload)
            {
                let loads = self
                    .cores
                    .iter()
                    .any(|core| intent.core == Some(core) || intent.cores.contains(core));
                if loads {
                    widening.loading.insert(intent.trial.clone());
                } else {
                    widening.loading.remove(&intent.trial);
                }
            }
            if self.evaluate(event, widening.as_mut()) {
                selected.push(event);
            }
        }
        selected
    }

    fn evaluate(&self, event: &Event, mut widening: Option<&mut Widening>) -> bool {
        let in_window = (self.kinds.is_empty()
            || self.kinds.iter().any(|kind| kind.matches(event.kind())))
            && self.since.is_none_or(|since| event.time >= since)
            && self.until.is_none_or(|until| event.time < until);
        if self.cores.is_empty() && self.trial.is_none() {
            return in_window;
        }
        if !in_window && widening.is_none() {
            return false;
        }
        let core_ok = self.cores.is_empty() || {
            let named = self.cores.iter().any(|core| event.payload.names_core(core));
            let of_trial = widening
                .as_deref()
                .is_some_and(|widening| widening_covers(&event.payload, widening));
            let ok = named || of_trial;
            if ok && let Some(widening) = widening.as_mut() {
                widening.matched.insert(event.seq);
            }
            ok
        };
        in_window
            && core_ok
            && self
                .trial
                .as_ref()
                .is_none_or(|wanted| event.payload.trial() == Some(wanted))
    }
}

fn widening_covers(payload: &Payload, widening: &Widening) -> bool {
    match payload {
        Payload::Failure(failure) => match failure.known_failure {
            Some(known) => widening.matched.contains(&known),
            None => {
                failure.core.is_none()
                    && failure
                        .trial
                        .as_ref()
                        .is_some_and(|trial| widening.loading.contains(trial))
            }
        },
        Payload::TrialEnd(end) => widening.loading.contains(&end.trial),
        Payload::SessionStart(_)
        | Payload::SessionContext(_)
        | Payload::SessionBaseline(_)
        | Payload::SessionNotice(_)
        | Payload::SessionWarning(_)
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
        | Payload::CrashDetected(_)
        | Payload::TunerDecision(_)
        | Payload::CorePhase(_)
        | Payload::CheckingCycle(_)
        | Payload::CheckingStep(_)
        | Payload::CheckingChain(_)
        | Payload::HostRanking(_)
        | Payload::HuntStart(_)
        | Payload::HuntGroup(_)
        | Payload::HuntEnd(_)
        | Payload::HuntSkipped(_)
        | Payload::Combination(_)
        | Payload::DeepeningRound(_)
        | Payload::TunerWarning(_)
        | Payload::BackendRetry(_)
        | Payload::CommandReset(_)
        | Payload::DefectFound(_)
        | Payload::DefectAnswered(_)
        | Payload::DeadEnd(_)
        | Payload::BootSavedEntry(_)
        | Payload::BootLeaveReason(_)
        | Payload::Shutdown(_)
        | Payload::JournalTorn(_)
        | Payload::DayBootStart(_)
        | Payload::DayCheckpoint(_)
        | Payload::DayOffsetsChanged(_)
        | Payload::DayCleanStop(_)
        | Payload::DayUncleanEnd(_)
        | Payload::DayAnswer(_)
        | Payload::DayRecord(_)
        | Payload::DayFault(_)
        | Payload::SessionImported(_)
        | Payload::ExposureObserved(_)
        | Payload::CcdEdge(_)
        | Payload::CcdCap(_)
        | Payload::NightPlan(_)
        | Payload::TaskStart(_)
        | Payload::NightQualification(_)
        | Payload::NightReproduction(_)
        | Payload::DayStep(_)
        | Payload::DayApplied(_)
        | Payload::DayEvent(_)
        | Payload::DayVeto(_)
        | Payload::DayHold(_)
        | Payload::DayFreeze(_)
        | Payload::DayInterval(_)
        | Payload::DayPause(_)
        | Payload::StockAttested(_)
        | Payload::DayImported(_)
        | Payload::UnitMapping(_)
        | Payload::UnitEdge(_)
        | Payload::ClimbDecision(_)
        | Payload::UnitStop(_)
        | Payload::CampaignStart(_)
        | Payload::CampaignCharge(_)
        | Payload::CampaignReclassified(_)
        | Payload::FirmwareRestoration(_)
        | Payload::SuccessorImported(_)
        | Payload::StateRebuilt(_) => false,
    }
}
