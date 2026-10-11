use std::collections::HashSet;
use std::str::FromStr;

use jiff::Timestamp;
use togi_machine::{CoreId, CoreSet};

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

#[derive(Clone, Debug, Default, PartialEq)]
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
            let named = self
                .cores
                .iter()
                .any(|core| names_core(&event.payload, core));
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
                .is_none_or(|wanted| trial_of(&event.payload) == Some(wanted))
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
        _ => false,
    }
}

fn trial_of(payload: &Payload) -> Option<&TrialId> {
    match payload {
        Payload::TrialIntent(p) => Some(&p.trial),
        Payload::TrialStart(p) => Some(&p.trial),
        Payload::TrialProgress(p) => Some(&p.trial),
        Payload::TrialSignal(p) => Some(&p.trial),
        Payload::TrialSample(p) => Some(&p.trial),
        Payload::TrialEnd(p) => Some(&p.trial),
        Payload::Failure(p) => p.trial.as_ref(),
        Payload::Mce(p) => p.trial.as_ref(),
        Payload::HuntStart(p) => p.trial.as_ref(),
        Payload::TunerWarning(p) => p.trial.as_ref(),
        Payload::SessionWarning(p) => p.trial.as_ref(),
        Payload::TrialCarried(p) => p.source.trial.as_ref(),
        Payload::FailureCarried(p) => p.source.trial.as_ref(),
        _ => None,
    }
}

fn names_core(payload: &Payload, core: CoreId) -> bool {
    match payload {
        Payload::SessionNotice(p) => p.cores.contains(core),
        Payload::SmuIntent(p) => p.command().core() == Some(core),
        Payload::SmuWrite(p) => p.command().core() == Some(core),
        Payload::SmuReadback(p) => p.core == core,
        Payload::SmuError(p) => p.core == Some(core),
        Payload::TrialIntent(p) => p.core == Some(core) || p.cores.contains(core),
        Payload::TrialStart(p) => p.instances.iter().any(|instance| instance.core == core),
        Payload::TrialProgress(p) => p.core == Some(core),
        Payload::TrialEnd(p) => p.core == Some(core),
        Payload::TrialCarried(p) => p.core == Some(core) || p.class.cores.contains(core),
        Payload::Failure(p) => p.core == Some(core),
        Payload::FailureCarried(p) => p.failure.core == Some(core) || p.class.cores.contains(core),
        Payload::Mce(p) => p.core == Some(core),
        Payload::TunerDecision(p) => p.core == core,
        Payload::CorePhase(p) => p.core == core,
        Payload::CheckingChain(p) => p.cores.contains(core),
        Payload::HuntStart(p) => p.cores.contains(core) || p.candidates.contains(core),
        Payload::HuntGroup(p) => {
            p.cores.contains(core) || p.probe.is_some_and(|probe| probe.core == core)
        }
        Payload::HuntEnd(p) => {
            p.cores.contains(core) || p.members.iter().any(|member| member.core == core)
        }
        Payload::Combination(p) => p.members.iter().any(|member| member.core == core),
        Payload::DeepeningRound(p) => p.cores.contains(core),
        Payload::CommandReset(p) => p.target == crate::ResetTarget::Core(core),
        Payload::DefectFound(p) => p.cores.contains(core),
        Payload::DefectAnswered(p) => p.cores.contains(core),
        Payload::DeadEnd(p) => p.core == Some(core),
        _ => false,
    }
}
