use std::collections::{BTreeMap, HashSet};

use jiff::Timestamp;
use togi_journal::{
    Decision, Event, HuntStage, Inference, Outcome, Payload, Seq, TrialIntent, WarningKind,
};
use togi_machine::{Regime, TargetOffset};
use togi_tuner::Profile;

use crate::project::{Class, Projection, Trial, loaded};
use crate::report::{hours, selected};
use crate::table::{Table, row};

/// The passes before `before` that still count for `target`: deeper than or as deep as it, of its class, and
/// after the latest failure at a profile no deeper than it. Idle crashes invalidate the passes of an all-core
/// R6 target too, since they fail every core at once.
pub(crate) fn prior_passes(
    p: &Projection<'_>,
    target: &TrialIntent,
    target_profile: Option<Profile>,
    before: Seq,
) -> usize {
    let Some(target_profile) = target_profile else {
        return 0;
    };
    let class = Class::of(target);
    let mut cutoff: Option<Seq> = None;
    let mut invalidate = |seq: Seq| cutoff = cutoff.max(Some(seq));
    for t in &p.trials {
        if let Some(end) = t.end
            && end.seq < before
            && t.class == class
            && end.end.outcome == Outcome::Failure
            && t.profile
                .is_some_and(|profile| profile.within(&target_profile))
        {
            invalidate(end.seq);
        }
    }
    if target.regime == Regime::R6 && loaded(target) == p.topology.all() {
        for idle in &p.idle {
            if idle.seq < before
                && idle
                    .profile
                    .is_some_and(|profile| profile.within(&target_profile))
            {
                invalidate(idle.seq);
            }
        }
    }
    p.trials
        .iter()
        .filter(|t| {
            t.end.is_some_and(|end| {
                Some(end.seq) > cutoff && end.seq < before && end.end.outcome == Outcome::Pass
            }) && t.class == class
                && t.profile
                    .is_some_and(|profile| profile.covers(&target_profile))
        })
        .count()
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
    for event in p.events.iter().filter(|e| selected(e.time, since)) {
        if let Payload::TunerWarning(warning) = &event.payload {
            *m.warnings.entry(warning.warning).or_default() += 1;
        }
    }
    for t in p
        .trials
        .iter()
        .filter(|t| selected(t.time, since) && t.failed())
    {
        if prior_passes(p, t.intent, t.profile, t.seq) > 0 {
            *m.contradictions.entry(t.class).or_default() += 1;
        }
    }
    for hunt in p.hunts.iter().filter(|h| selected(h.time, since)) {
        for group in hunt.groups.iter().filter(|g| !g.trials.is_empty()) {
            let first = &p.trials[group.trials[0]];
            let mut g = GroupReuse {
                hunt: hunt.id,
                group: group.plan.group.get(),
                stage: group.plan.stage,
                prior: prior_passes(p, first.intent, first.profile, hunt.seq),
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
            _ => false,
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
                _ => pending.extend_from_slice(&source.cause),
            }
        }
        if failures == 1 && carried {
            count += 1;
        }
    }
    count
}

/// The event numbered `seq`: a journal numbers its events from 1 without gaps.
pub(crate) fn event_at(events: &[Event], seq: Seq) -> Option<&Event> {
    let at = usize::try_from(seq.get().checked_sub(1)?).ok()?;
    events.get(at).filter(|event| event.seq == seq)
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
