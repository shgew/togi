use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::Path;

use jiff::Timestamp;
use togi_journal::{Decision, Event, HuntStage, Inference, Outcome, Payload, Seq, WarningKind};
use togi_machine::{Backend, TargetOffset};
use togi_tuner::{ClassKey, Evidence, Observation, Profile, Secs, SessionRef, TopologyMismatch};

use crate::project::{Class, Projection, Trial};
use crate::report::{hours, selected};
use crate::table::{Table, row};

/// Fold recorded observations once, asking the tuner at each requested historical boundary.
/// Profiles are already checked against this projection's topology.
pub(crate) fn pass_counts(
    p: &Projection<'_>,
    mut requests: Vec<(Seq, usize)>,
) -> Result<BTreeMap<(Seq, usize), usize>, TopologyMismatch> {
    requests.sort_unstable();
    requests.dedup();
    let mut requests = requests.into_iter().peekable();
    let mut counts = BTreeMap::new();
    let mut evidence = Evidence::new(p.topology.all());
    let mut backends = HashMap::new();
    for event in p.events {
        while let Some(&(before, at)) = requests.peek()
            && before <= event.seq
        {
            let trial = &p.trials[at];
            let count = match (key(trial.class), trial.profile) {
                (Some(class), Some(profile)) => evidence.passes_before(class, &profile, before)?,
                _ => 0,
            };
            counts.insert((before, at), count);
            requests.next();
        }
        if requests.peek().is_none() {
            break;
        }
        match &event.payload {
            Payload::ConfigLoaded(loaded) => {
                for (backend, path) in [
                    (Backend::Mprime, &loaded.config.backends.mprime),
                    (Backend::Ycruncher, &loaded.config.backends.ycruncher),
                ] {
                    evidence.set_backend(backend, path.as_deref().map(Path::new));
                }
            }
            Payload::TrialIntent(intent) => {
                backends.insert(
                    &intent.trial,
                    evidence.active_backend(intent.workload.backend()),
                );
            }
            Payload::TrialEnd(end) => {
                let Some(&at) = p.by_id.get(&end.trial) else {
                    continue;
                };
                let trial = &p.trials[at];
                let (Some(class), Some(profile)) = (key(trial.class), trial.profile) else {
                    continue;
                };
                let obs = Observation {
                    seq: event.seq,
                    class,
                    profile: &profile,
                    condition: trial.intent.condition,
                    named: end.core,
                };
                let backend = backends.remove(&end.trial);
                match end.outcome {
                    Outcome::Pass => {
                        if let Some(backend) = backend {
                            evidence.record_pass(obs, backend)?;
                        }
                    }
                    Outcome::Failure => evidence.record_failure(obs)?,
                    Outcome::Inconclusive => {}
                }
            }
            Payload::TrialCarried(carried) => {
                let workload = carried
                    .class
                    .workload
                    .unwrap_or_else(|| togi_machine::workloads(carried.class.regime)[0].id());
                let Some(duration) = NonZeroU32::new(carried.class.duration_s) else {
                    continue;
                };
                let Ok(profile) = Profile::new(p.topology, carried.profile) else {
                    continue;
                };
                let obs = Observation {
                    seq: event.seq,
                    class: ClassKey::new(workload, carried.class.cores, Secs::new(duration)),
                    profile: &profile,
                    condition: carried.condition,
                    named: carried.core,
                };
                let source = SessionRef::new(carried.source.session.to_string());
                match carried.outcome {
                    Outcome::Pass => {
                        let backend = match &carried.source.backend {
                            Some(id) => evidence.backend_id(id.backend, Some(Path::new(&id.path))),
                            None => evidence.backend_id(workload.backend(), None),
                        };
                        evidence.record_carried_pass(obs, backend, source)?;
                    }
                    Outcome::Failure => evidence.record_carried_failure(obs, source)?,
                    Outcome::Inconclusive => {}
                }
            }
            Payload::FailureCarried(carried) => {
                if let Some(offsets) = carried.failure.profile
                    && let Ok(profile) = Profile::new(p.topology, offsets)
                {
                    evidence.record_carried_idle_failure(
                        event.seq,
                        &profile,
                        SessionRef::new(carried.source.session.to_string()),
                    )?;
                }
            }
            Payload::CommandReset(reset) => {
                if let togi_journal::ResetTarget::Core(core) = reset.target {
                    evidence.reset_core(core);
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
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::Failure(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
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
            | Payload::TunerDecision(_)
            | Payload::BackendRetry(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {}
        }
        if let Ok(at) = p.idle.binary_search_by_key(&event.seq, |idle| idle.seq)
            && let Some(profile) = p.idle[at].profile
        {
            evidence.record_idle_failure(event.seq, &profile)?;
        }
    }
    Ok(counts)
}

fn key(class: Class) -> Option<ClassKey> {
    Some(ClassKey::new(
        class.workload,
        class.cores,
        Secs::new(NonZeroU32::new(class.duration_s)?),
    ))
}

/// One hunt group's prior evidence and what running it found.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct GroupReuse {
    pub(crate) hunt: u32,
    pub(crate) group: u32,
    pub(crate) stage: HuntStage,
    pub(crate) prior: usize,
    pub(crate) required: u32,
    pub(crate) established: bool,
    pub(crate) passes: u32,
    pub(crate) failures: u32,
    pub(crate) trials: usize,
    pub(crate) seconds: u32,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct StageReuse {
    pub(crate) groups: u32,
    pub(crate) established: u32,
    pub(crate) passed: u32,
    pub(crate) failed: u32,
    pub(crate) trials: usize,
    pub(crate) seconds: u32,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct EvidenceMetrics {
    pub(crate) warnings: BTreeMap<WarningKind, u32>,
    pub(crate) single_carried: u32,
    pub(crate) contradictions: BTreeMap<Class, u32>,
    pub(crate) groups: Vec<GroupReuse>,
    pub(crate) stages: BTreeMap<(u32, HuntStage), StageReuse>,
}

pub(crate) fn compute(p: &Projection<'_>, since: Option<Timestamp>) -> EvidenceMetrics {
    let mut m = EvidenceMetrics::default();
    let requests: Vec<_> = p
        .trials
        .iter()
        .enumerate()
        .filter(|(_, trial)| selected(trial.time, since) && trial.failed())
        .map(|(at, trial)| (trial.seq, at))
        .chain(
            p.hunts
                .iter()
                .filter(|hunt| selected(hunt.time, since))
                .flat_map(|hunt| {
                    hunt.groups
                        .iter()
                        .filter_map(move |group| group.trials.first().map(|&at| (hunt.seq, at)))
                }),
        )
        .collect();
    let counts = pass_counts(p, requests).unwrap_or_else(|error| {
        unreachable!("projection profiles share the evidence topology: {error}")
    });
    for event in p.events.iter().filter(|e| selected(e.time, since)) {
        if let Payload::TunerWarning(warning) = &event.payload {
            *m.warnings.entry(warning.warning).or_default() += 1;
        }
    }
    for (at, t) in p
        .trials
        .iter()
        .enumerate()
        .filter(|(_, t)| selected(t.time, since) && t.failed())
    {
        if counts[&(t.seq, at)] > 0 {
            *m.contradictions.entry(t.class).or_default() += 1;
        }
    }
    for hunt in p.hunts.iter().filter(|h| selected(h.time, since)) {
        for group in hunt.groups.iter().filter(|g| !g.trials.is_empty()) {
            let mut g = GroupReuse {
                hunt: hunt.id,
                group: group.plan.group.get(),
                stage: group.plan.stage,
                prior: counts[&(hunt.seq, group.trials[0])],
                required: hunt.start.trials,
                established: false,
                passes: 0,
                failures: 0,
                trials: group.trials.len(),
                seconds: 0,
            };
            g.established = g.prior >= usize::try_from(g.required).unwrap_or(usize::MAX);
            for &at in &group.trials {
                let t = &p.trials[at];
                g.seconds += t.seconds();
                match t.outcome() {
                    Some(Outcome::Pass) => g.passes += 1,
                    Some(Outcome::Failure) => g.failures += 1,
                    Some(Outcome::Inconclusive) | None => {}
                }
            }
            let s = m.stages.entry((g.hunt, group.plan.stage)).or_default();
            s.groups += 1;
            if g.established {
                s.established += 1;
                if g.failures > 0 {
                    s.failed += 1;
                } else if g.passes >= g.required {
                    s.passed += 1;
                }
                s.trials += g.trials;
                s.seconds += g.seconds;
            }
            m.groups.push(g);
        }
    }
    m.single_carried = single_carried_failure_decisions(p.events, since);
    m
}

pub(crate) fn render(table: &mut Table, m: &EvidenceMetrics) {
    table.section("Evidence quality", &["warning", "count"]);
    for (warning, count) in &m.warnings {
        row!(table, warning, count);
    }
    row!(
        table,
        "decisions resting on a single carried failure",
        m.single_carried
    );
    table.section(
        "Failures after prior passes",
        &["trial class", "failures with prior passes"],
    );
    for (class, count) in &m.contradictions {
        row!(table, class.label(), count);
    }
    table.section(
        "Prior evidence per hunt group",
        &[
            "hunt",
            "group",
            "stage",
            "prior passes",
            "required",
            "established",
            "passes again",
            "failures again",
            "trials",
            "hours",
        ],
    );
    for g in &m.groups {
        row!(
            table,
            g.hunt,
            g.group,
            g.stage,
            g.prior,
            g.required,
            g.established,
            g.passes,
            g.failures,
            g.trials,
            hours(f64::from(g.seconds))
        );
    }
    table.section(
        "Prior evidence by hunt and stage",
        &[
            "hunt",
            "stage",
            "ran groups",
            "established groups",
            "passed again",
            "failed again",
            "redundant trials",
            "redundant hours",
        ],
    );
    for ((hunt, stage), s) in &m.stages {
        row!(
            table,
            hunt,
            stage,
            s.groups,
            s.established,
            s.passed,
            s.failed,
            s.trials,
            hours(f64::from(s.seconds))
        );
    }
}

/// Counts the inferred-failure hunt groups and the backoffs whose causes reach exactly one distinct failure
/// observation, and that one is carried. Observations end a branch: a failed carried trial, a carried idle
/// failure, an idle failure and a failed trial end each count one failure; every other event passes its causes
/// on. A cause reached twice counts once.
pub(crate) fn single_carried_failure_decisions(events: &[Event], since: Option<Timestamp>) -> u32 {
    let mut count = 0;
    for event in events.iter().filter(|e| selected(e.time, since)) {
        let decided = match &event.payload {
            Payload::TunerDecision(decision) => decision.decision == Decision::Backoff,
            Payload::HuntGroup(group) => group.inferred == Some(Inference::Failure),
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
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
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
            | Payload::StateRebuilt(_) => false,
        };
        if !decided {
            continue;
        }
        let (mut failures, mut carried) = (0, false);
        let mut seen: HashSet<Seq> = HashSet::new();
        let mut pending = event.cause.clone();
        while let Some(seq) = pending.pop() {
            if !seen.insert(seq) {
                continue;
            }
            let Some(source) = event_at(events, seq) else {
                continue;
            };
            match &source.payload {
                Payload::TrialCarried(c) if c.outcome == Outcome::Failure => {
                    failures += 1;
                    carried = true;
                }
                Payload::FailureCarried(_) => {
                    failures += 1;
                    carried = true;
                }
                Payload::Failure(_) => {
                    failures += 1;
                    carried = false;
                }
                Payload::TrialEnd(end) if end.outcome == Outcome::Failure => {
                    failures += 1;
                    carried = false;
                }
                Payload::TrialCarried(_) | Payload::TrialEnd(_) => {}
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
                | Payload::StateRebuilt(_) => pending.extend_from_slice(&source.cause),
            }
        }
        if failures == 1 && carried {
            count += 1;
        }
    }
    count
}

/// The event numbered `seq`; none for a number a lenient read skipped. Events ascend by seq.
pub(crate) fn event_at(events: &[Event], seq: Seq) -> Option<&Event> {
    let at = events.binary_search_by_key(&seq, |event| event.seq).ok()?;
    events.get(at)
}

/// A multi-core class's trials counted by the shallowest loaded offset.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DepthRate {
    pub(crate) class: Class,
    pub(crate) depth: i64,
    pub(crate) trials: u32,
    pub(crate) failures: u32,
}

/// The trials of one class by depth, and whether any failed.
#[derive(Default)]
struct DepthClass {
    depths: BTreeMap<i64, (u32, u32)>,
    failing: bool,
}

pub(crate) fn depth(p: &Projection<'_>, since: Option<Timestamp>) -> Vec<DepthRate> {
    let mut classes: BTreeMap<Class, DepthClass> = BTreeMap::new();
    for t in p
        .trials
        .iter()
        .filter(|t| selected(t.time, since) && t.started && t.class.cores.len() >= 2)
    {
        let Some(depth) = shallowest(t) else {
            continue;
        };
        let class = classes.entry(t.class).or_default();
        let (trials, failures) = class.depths.entry(depth).or_default();
        *trials += 1;
        if t.failed() {
            *failures += 1;
            class.failing = true;
        }
    }
    classes
        .into_iter()
        .filter(|(_, class)| class.failing)
        .flat_map(|(class, depths)| {
            depths
                .depths
                .into_iter()
                .map(move |(depth, (trials, failures))| DepthRate {
                    class,
                    depth,
                    trials,
                    failures,
                })
        })
        .collect()
}

/// The offset of the loaded core closest to zero; `None` when the profile misses a loaded core.
fn shallowest(t: &Trial<'_>) -> Option<i64> {
    t.class
        .cores
        .iter()
        .map(|core| t.intent.profile.get(core).map(TargetOffset::get))
        .try_fold(i64::MIN, |deepest, offset| Some(deepest.max(offset?)))
}

pub(crate) fn render_depth(table: &mut Table, rates: &[DepthRate]) {
    table.section(
        "Failure rate by depth",
        &[
            "regime / workload / loaded cores / duration",
            "shallowest offset",
            "trials",
            "failures",
            "rate",
        ],
    );
    for r in rates {
        row!(
            table,
            r.class.label(),
            r.depth,
            r.trials,
            r.failures,
            format!(
                "{:.1}%",
                100.0 * f64::from(r.failures) / f64::from(r.trials)
            )
        );
    }
}
