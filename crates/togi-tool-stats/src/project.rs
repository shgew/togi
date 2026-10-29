use std::collections::HashMap;
use std::fmt::Write as _;

use jiff::Timestamp;
use togi_journal::{Event, Outcome, Payload, Seq, ShutdownReason, TrialEnd, TrialId, TrialIntent};
use togi_machine::{BootId, CoreId, CoreSet, Signal, WorkloadId};

use crate::journal::Journal;

/// What trials are grouped by: the workload, the loaded cores and the intended duration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Class {
    pub(crate) workload: WorkloadId,
    pub(crate) cores: CoreSet,
    pub(crate) duration_s: u32,
}

impl Class {
    pub(crate) fn of(intent: &TrialIntent) -> Self {
        Self {
            workload: intent.workload,
            cores: loaded(intent),
            duration_s: intent.duration_s,
        }
    }

    /// `R7/workload/00,01/120s`.
    pub(crate) fn label(self) -> String {
        format!(
            "{}/{}/{}/{}s",
            self.workload.regime(),
            self.workload,
            core_list(self.cores),
            self.duration_s
        )
    }
}

impl Ord for Class {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.workload
            .cmp(&other.workload)
            .then_with(|| {
                self.cores
                    .iter()
                    .map(CoreId::get)
                    .cmp(other.cores.iter().map(CoreId::get))
            })
            .then(self.duration_s.cmp(&other.duration_s))
    }
}

impl PartialOrd for Class {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The cores a trial loads: the one it names (which wins if both fields are recorded), or its set.
pub(crate) fn loaded(intent: &TrialIntent) -> CoreSet {
    match intent.core {
        Some(core) => std::iter::once(core).collect(),
        None => intent.cores,
    }
}

/// Two-digit core numbers, comma separated.
pub(crate) fn core_list(cores: CoreSet) -> String {
    let mut list = String::new();
    for (i, core) in cores.iter().enumerate() {
        if i > 0 {
            list.push(',');
        }
        let _ = write!(list, "{:02}", core.get());
    }
    list
}

pub(crate) struct Trial<'a> {
    pub(crate) time: Timestamp,
    pub(crate) boot: &'a BootId,
    pub(crate) intent: &'a TrialIntent,
    pub(crate) class: Class,
    pub(crate) started: bool,
    pub(crate) last_evidence: Timestamp,
    pub(crate) crashed: bool,
    pub(crate) end: Option<End<'a>>,
}

#[derive(Clone, Copy)]
pub(crate) struct End<'a> {
    pub(crate) end: &'a TrialEnd,
}

impl Trial<'_> {
    pub(crate) fn outcome(&self) -> Option<Outcome> {
        self.end.map(|end| end.end.evidence_outcome())
    }

    pub(crate) fn failed(&self) -> bool {
        self.outcome() == Some(Outcome::Failure)
    }

    pub(crate) fn seconds(&self) -> u32 {
        self.end.map_or(0, |end| end.end.duration_s)
    }

    /// Interrupted by a crash: the next boot said so, or the trial ended as a crash failure.
    pub(crate) fn crash(&self) -> bool {
        self.crashed
            || self.failed()
                && self
                    .end
                    .is_some_and(|e| e.end.signal == Some(Signal::Crash))
    }
}

pub(crate) struct Run {
    pub(crate) start: Timestamp,
    pub(crate) end: Timestamp,
    pub(crate) builds: std::collections::BTreeMap<String, u32>,
    pub(crate) trials: u32,
    pub(crate) crashes: u32,
    /// `None` while the run has not shut down.
    pub(crate) ending: Option<ShutdownReason>,
    boot: BootId,
}

pub(crate) struct BootTime {
    pub(crate) start: Timestamp,
    pub(crate) first_event: Timestamp,
}

/// A journal's trials, runs and boots, joined by the events' own references.
pub(crate) struct Projection<'a> {
    pub(crate) events: &'a [Event],
    pub(crate) journal: &'a Journal,
    pub(crate) trials: Vec<Trial<'a>>,
    pub(crate) by_id: HashMap<&'a TrialId, usize>,
    pub(crate) runs: Vec<Run>,
    pub(crate) boots: HashMap<&'a BootId, BootTime>,
    pub(crate) next_boot: HashMap<&'a BootId, &'a BootId>,
}

impl<'a> Projection<'a> {
    pub(crate) fn new(journal: &'a Journal) -> Self {
        let mut p = Self {
            events: journal.events(),
            journal,
            trials: Vec::new(),
            by_id: HashMap::new(),
            runs: Vec::new(),
            boots: HashMap::new(),
            next_boot: HashMap::new(),
        };
        p.fold(journal);
        p
    }

    fn fold(&mut self, journal: &Journal) {
        let mut active: Option<usize> = None;
        let mut by_seq: HashMap<Seq, usize> = HashMap::new();
        let mut previous_boot: Option<&BootId> = None;
        for event in self.events {
            if !self.boots.contains_key(&event.boot) {
                let start = journal.boot_start(&event.boot);
                self.boots.insert(
                    &event.boot,
                    BootTime {
                        start,
                        first_event: event.time,
                    },
                );
                if let Some(previous) = previous_boot {
                    self.next_boot.insert(previous, &event.boot);
                }
                previous_boot = Some(&event.boot);
            }
            if let Some(run) = active {
                self.runs[run].end = event.time;
            }
            match &event.payload {
                Payload::ConfigLoaded(loaded) => {
                    let continues = active.is_some_and(|run| self.runs[run].boot != event.boot);
                    let run = match active {
                        Some(run) if continues => run,
                        _ => {
                            self.runs.push(Run {
                                start: event.time,
                                end: event.time,
                                builds: std::collections::BTreeMap::new(),
                                trials: 0,
                                crashes: 0,
                                ending: None,
                                boot: event.boot.clone(),
                            });
                            self.runs.len() - 1
                        }
                    };
                    active = Some(run);
                    self.runs[run].boot.clone_from(&event.boot);
                    *self.runs[run]
                        .builds
                        .entry(build_name(&loaded.build))
                        .or_default() += 1;
                }
                Payload::Shutdown(shutdown) => {
                    if let Some(run) = active.take() {
                        self.runs[run].ending = Some(shutdown.reason);
                        self.runs[run].end = event.time;
                    }
                }
                Payload::TrialIntent(intent) => {
                    let at = self.trials.len();
                    self.trials.push(Trial {
                        time: event.time,
                        boot: &event.boot,
                        intent,
                        class: Class::of(intent),
                        started: false,
                        last_evidence: event.time,
                        crashed: false,
                        end: None,
                    });
                    by_seq.insert(event.seq, at);
                    self.by_id.insert(&intent.trial, at);
                    if let Some(run) = active {
                        self.runs[run].trials += 1;
                    }
                }
                Payload::TrialStart(start) => {
                    if let Some(&at) = self.by_id.get(&start.trial) {
                        self.trials[at].started = true;
                        self.trials[at].last_evidence = event.time;
                    }
                }
                Payload::TrialProgress(p) => self.evidence(&p.trial, event),
                Payload::TrialSignal(s) => self.evidence(&s.trial, event),
                Payload::TrialSample(s) => self.evidence(&s.trial, event),
                Payload::TrialEnd(end) => {
                    if let Some(&at) = self.by_id.get(&end.trial) {
                        self.trials[at].end = Some(End { end });
                    }
                }
                Payload::CrashDetected(crash) => {
                    if let Some(run) = active {
                        self.runs[run].crashes += 1;
                    }
                    if let Some(&at) = crash.in_flight.and_then(|seq| by_seq.get(&seq))
                        && self.trials[at].boot == &crash.previous_boot
                    {
                        self.trials[at].crashed = true;
                    }
                }
                Payload::SessionStart(_)
                | Payload::SessionContext(_)
                | Payload::SessionBaseline(_)
                | Payload::SessionNotice(_)
                | Payload::SessionWarning(_)
                | Payload::SessionArchived(_)
                | Payload::SessionCarried(_)
                | Payload::PreflightCheck(_)
                | Payload::SmuIntent(_)
                | Payload::SmuWrite(_)
                | Payload::SmuReadback(_)
                | Payload::SmuError(_)
                | Payload::ProfileApplied(_)
                | Payload::ProfileChange(_)
                | Payload::ProfileRestored(_)
                | Payload::TrialCarried(_)
                | Payload::FailureCarried(_)
                | Payload::Failure(_)
                | Payload::Mce(_)
                | Payload::TunerDecision(_)
                | Payload::HuntStart(_)
                | Payload::HuntGroup(_)
                | Payload::HuntEnd(_)
                | Payload::Combination(_)
                | Payload::CorePhase(_)
                | Payload::CheckingCycle(_)
                | Payload::CheckingStep(_)
                | Payload::CheckingChain(_)
                | Payload::HostRanking(_)
                | Payload::HuntSkipped(_)
                | Payload::DeepeningRound(_)
                | Payload::TunerWarning(_)
                | Payload::BackendRetry(_)
                | Payload::TaskStart(_)
                | Payload::CommandReset(_)
                | Payload::DefectFound(_)
                | Payload::DefectAnswered(_)
                | Payload::DeadEnd(_)
                | Payload::BootSavedEntry(_)
                | Payload::BootLeaveReason(_)
                | Payload::JournalTorn(_)
                | Payload::DayBootStart(_)
                | Payload::DayCheckpoint(_)
                | Payload::DayOffsetsChanged(_)
                | Payload::DayCleanStop(_)
                | Payload::DayAnswer(_)
                | Payload::DayRecord(_)
                | Payload::DayFault(_)
                | Payload::SessionImported(_)
                | Payload::ExposureObserved(_)
                | Payload::CcdEdge(_)
                | Payload::CcdCap(_)
                | Payload::NightPlan(_)
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
                | Payload::DayImported(_)
                | Payload::DayUncleanEnd(_)
                | Payload::StateRebuilt(_) => {}
            }
        }
    }

    fn evidence(&mut self, trial: &TrialId, event: &Event) {
        if let Some(&at) = self.by_id.get(trial) {
            let t = &mut self.trials[at];
            if t.started && t.boot == &event.boot && t.end.is_none() {
                t.last_evidence = event.time;
            }
        }
    }

    pub(crate) fn ccd_of(&self, core: CoreId) -> Option<togi_machine::CcdId> {
        self.journal.ccd_of(core)
    }
}

/// `1.2.3+abc`, or `unstamped` for a build without a version.
pub(crate) fn build_name(build: &togi_journal::Build) -> String {
    match (build.version.as_str(), build.rev.as_deref()) {
        ("", _) => "unstamped".to_owned(),
        (version, None | Some("")) => version.to_owned(),
        (version, Some(rev)) => format!("{version}+{rev}"),
    }
}
