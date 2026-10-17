use std::collections::HashMap;
use std::fmt::Write as _;

use jiff::{SignedDuration, Timestamp};
use togi_journal::{
    Event, HuntEnd, HuntGroup, HuntStart, Outcome, Payload, Seq, ShutdownReason, TrialEnd, TrialId,
    TrialIntent,
};
use togi_machine::{BootId, CoreId, CoreSet, Signal, WorkloadId};
use togi_tuner::{Profile, Topology};

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

/// The cores a trial loads: the one it names, or its set.
pub(crate) fn loaded(intent: &TrialIntent) -> CoreSet {
    intent
        .core
        .map_or(intent.cores, |core| CoreSet::from_iter([core]))
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
    pub(crate) seq: Seq,
    pub(crate) time: Timestamp,
    pub(crate) boot: &'a BootId,
    pub(crate) cause: &'a [Seq],
    pub(crate) intent: &'a TrialIntent,
    pub(crate) profile: Option<Profile>,
    pub(crate) class: Class,
    pub(crate) started: bool,
    pub(crate) last_evidence: Timestamp,
    pub(crate) crashed: bool,
    pub(crate) end: Option<End<'a>>,
}

#[derive(Clone, Copy)]
pub(crate) struct End<'a> {
    pub(crate) seq: Seq,
    pub(crate) end: &'a TrialEnd,
}

impl Trial<'_> {
    pub(crate) fn outcome(&self) -> Option<Outcome> {
        self.end.map(|end| end.end.outcome)
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

pub(crate) struct Group<'a> {
    pub(crate) plan: &'a HuntGroup,
    pub(crate) trials: Vec<usize>,
}

pub(crate) struct Hunt<'a> {
    pub(crate) start: &'a HuntStart,
    pub(crate) id: u32,
    pub(crate) time: Timestamp,
    pub(crate) seq: Seq,
    pub(crate) end_time: Timestamp,
    pub(crate) result: Option<&'a HuntEnd>,
    pub(crate) commitment: Option<String>,
    pub(crate) groups: Vec<Group<'a>>,
    pub(crate) trials: Vec<usize>,
}

pub(crate) struct BootTime {
    pub(crate) start: Timestamp,
    pub(crate) first_event: Timestamp,
}

pub(crate) struct IdleFailure {
    pub(crate) seq: Seq,
    pub(crate) profile: Option<Profile>,
}

/// A journal's trials, runs, hunts and boots, joined by the events' own references.
pub(crate) struct Projection<'a> {
    pub(crate) events: &'a [Event],
    pub(crate) topology: Topology,
    pub(crate) trials: Vec<Trial<'a>>,
    pub(crate) by_id: HashMap<&'a TrialId, usize>,
    pub(crate) idle: Vec<IdleFailure>,
    pub(crate) runs: Vec<Run>,
    pub(crate) hunts: Vec<Hunt<'a>>,
    pub(crate) boots: HashMap<&'a BootId, BootTime>,
    pub(crate) next_boot: HashMap<&'a BootId, &'a BootId>,
}

impl<'a> Projection<'a> {
    pub(crate) fn new(journal: &'a Journal) -> Self {
        let mut p = Self {
            events: journal.events(),
            topology: journal.topology().clone(),
            trials: Vec::new(),
            by_id: HashMap::new(),
            idle: Vec::new(),
            runs: Vec::new(),
            hunts: Vec::new(),
            boots: HashMap::new(),
            next_boot: HashMap::new(),
        };
        p.fold();
        p
    }

    fn fold(&mut self) {
        let mut active: Option<usize> = None;
        let mut by_seq: HashMap<Seq, usize> = HashMap::new();
        let mut hunt_ids: HashMap<u32, usize> = HashMap::new();
        let mut group_at: HashMap<(u32, u32), (usize, usize)> = HashMap::new();
        let mut commitments: HashMap<Seq, usize> = HashMap::new();
        let mut previous_boot: Option<&BootId> = None;
        for event in self.events {
            if !self.boots.contains_key(&event.boot) {
                let up = event
                    .mono
                    .and_then(|mono| SignedDuration::try_from(mono).ok());
                let start = up
                    .and_then(|up| event.time.checked_sub(up).ok())
                    .unwrap_or(event.time);
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
                        seq: event.seq,
                        time: event.time,
                        boot: &event.boot,
                        cause: &event.cause,
                        intent,
                        profile: Profile::new(&self.topology, intent.profile).ok(),
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
                    if let Some(&hunt) = intent.hunt.and_then(|id| hunt_ids.get(&id.get())) {
                        self.hunts[hunt].trials.push(at);
                    }
                    if let (Some(hunt), Some(group)) = (intent.hunt, intent.group)
                        && let Some(&(h, g)) = group_at.get(&(hunt.get(), group.get()))
                    {
                        self.hunts[h].groups[g].trials.push(at);
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
                        self.trials[at].end = Some(End {
                            seq: event.seq,
                            end,
                        });
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
                Payload::Failure(failure) => {
                    if failure.known_failure.is_none() && failure.trial.is_none() {
                        self.idle.push(IdleFailure {
                            seq: event.seq,
                            profile: failure
                                .profile
                                .and_then(|offsets| Profile::new(&self.topology, offsets).ok()),
                        });
                    }
                }
                Payload::HuntStart(start) => {
                    let id = start.hunt.get();
                    hunt_ids.insert(id, self.hunts.len());
                    self.hunts.push(Hunt {
                        start,
                        id,
                        time: event.time,
                        seq: event.seq,
                        end_time: event.time,
                        result: None,
                        commitment: None,
                        groups: Vec::new(),
                        trials: Vec::new(),
                    });
                }
                Payload::HuntGroup(plan) => {
                    if let Some(&h) = hunt_ids.get(&plan.hunt.get()) {
                        group_at.insert(
                            (plan.hunt.get(), plan.group.get()),
                            (h, self.hunts[h].groups.len()),
                        );
                        self.hunts[h].groups.push(Group {
                            plan,
                            trials: Vec::new(),
                        });
                    }
                }
                Payload::HuntEnd(end) => {
                    if let Some(&h) = hunt_ids.get(&end.hunt.get()) {
                        self.hunts[h].end_time = event.time;
                        self.hunts[h].result = Some(end);
                        commitments.insert(event.seq, h);
                    }
                }
                Payload::Combination(combination) => {
                    if let Some(&h) = hunt_ids.get(&combination.hunt.get()) {
                        commitments.insert(event.seq, h);
                    }
                }
                Payload::TunerDecision(decision)
                    if decision.decision == togi_journal::Decision::Backoff =>
                {
                    if let Some(&h) = event.cause.iter().find_map(|cause| commitments.get(cause)) {
                        self.hunts[h].commitment = Some(format!(
                            "{:02} {}->{}",
                            decision.core.get(),
                            decision.from_offset.get(),
                            decision.to_offset.get()
                        ));
                    }
                }
                _ => {}
            }
        }
        if let Some(last) = self.events.last() {
            for hunt in self.hunts.iter_mut().filter(|hunt| hunt.result.is_none()) {
                hunt.end_time = last.time;
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
        self.topology.ccd_of(core)
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
