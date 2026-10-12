use std::collections::{BTreeMap, HashSet};

use jiff::Timestamp;
use togi_journal::{Activity, CycleEvent, Outcome, Payload, Seq};
use togi_machine::{Condition, Regime};

use crate::project::{Hunt, Projection, core_list};
use crate::report::{hours, outcome_name, selected, stamp};
use crate::table::{Table, row};

pub(crate) struct HuntMetrics<'a> {
    pub(crate) hunt: &'a Hunt<'a>,
    pub(crate) ran: usize,
    pub(crate) inferred: usize,
    pub(crate) skipped: usize,
    pub(crate) crashes: usize,
}

pub(crate) fn compute_hunts<'a>(
    p: &'a Projection<'a>,
    since: Option<Timestamp>,
) -> Vec<HuntMetrics<'a>> {
    p.hunts
        .iter()
        .filter(|hunt| selected(hunt.time, since))
        .map(|hunt| HuntMetrics {
            hunt,
            ran: hunt.groups.iter().filter(|g| !g.trials.is_empty()).count(),
            inferred: hunt
                .groups
                .iter()
                .filter(|g| g.plan.inferred.is_some())
                .count(),
            skipped: hunt.groups.iter().filter(|g| g.plan.skipped).count(),
            crashes: hunt.trials.iter().filter(|&&t| p.trials[t].crash()).count(),
        })
        .collect()
}

/// How a hunt's parked offsets came about: the passed full cycle's end (`#<seq>`), a located hunt holding its
/// loaded cores at their failing offsets (`loaded-held`), or every core at zero.
pub(crate) fn parked_label(hunt: &Hunt<'_>) -> String {
    if let Some(seq) = hunt.start.parked_seq {
        format!("#{seq}")
    } else if hunt
        .start
        .parked
        .iter()
        .any(|(_, offset)| offset.get() != 0)
    {
        "loaded-held".to_owned()
    } else {
        "all-zero".to_owned()
    }
}

pub(crate) fn render_hunts(table: &mut Table, hunts: &[HuntMetrics<'_>]) {
    table.section(
        "Hunts",
        &[
            "hunt",
            "start",
            "hours",
            "failing trial",
            "parked offsets",
            "candidates",
            "planned/run/inferred/skipped",
            "trials",
            "crashes",
            "result",
            "members/culprit",
            "commitment",
        ],
    );
    for m in hunts {
        let h = m.hunt;
        let (result, members) = match h.result {
            None => ("open".to_owned(), "-".to_owned()),
            Some(end) if end.members.is_empty() => {
                let cores = core_list(end.cores);
                (
                    end.result.to_string(),
                    if cores.is_empty() {
                        "-".to_owned()
                    } else {
                        cores
                    },
                )
            }
            Some(end) => {
                let mut members: Vec<_> = end.members.iter().collect();
                members.sort_by_key(|member| member.core);
                let labels: Vec<String> = members
                    .iter()
                    .map(|member| format!("{:02}:{}", member.core.get(), member.offset.get()))
                    .collect();
                (end.result.to_string(), labels.join(","))
            }
        };
        row!(
            table,
            h.id,
            stamp(h.time),
            hours(h.end_time.duration_since(h.time).as_secs_f64()),
            h.start
                .trial
                .as_ref()
                .map_or_else(|| "-".to_owned(), ToString::to_string),
            parked_label(h),
            h.start.candidates.len(),
            format!("{}/{}/{}/{}", h.groups.len(), m.ran, m.inferred, m.skipped),
            h.trials.len(),
            m.crashes,
            result,
            members,
            h.commitment.as_deref().unwrap_or("none")
        );
    }
}

/// A checking step: `cycle` is zero for reruns, which belong to no cycle.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct StepKey {
    pub(crate) rerun: bool,
    pub(crate) cycle: u32,
    pub(crate) regime: Regime,
    pub(crate) cores: String,
    pub(crate) outcome: Option<Outcome>,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct CheckingMetrics {
    pub(crate) cycles_started: u32,
    pub(crate) cycles_ended: u32,
    pub(crate) full_cycles: u32,
    pub(crate) reruns: u32,
    pub(crate) reruns_failing: u32,
    pub(crate) steps: BTreeMap<StepKey, u32>,
}

pub(crate) fn compute_checking(p: &Projection<'_>, since: Option<Timestamp>) -> CheckingMetrics {
    let mut m = CheckingMetrics::default();
    for event in p.events.iter().filter(|e| selected(e.time, since)) {
        if let Payload::CheckingCycle(cycle) = &event.payload {
            match cycle.event {
                CycleEvent::Start => m.cycles_started += 1,
                CycleEvent::End => {
                    m.cycles_ended += 1;
                    m.full_cycles += u32::from(cycle.full);
                }
            }
        }
    }
    let mut obligations: HashSet<Seq> = HashSet::new();
    for t in p.trials.iter().filter(|t| t.intent.rerun) {
        let obligation = t.cause.first().copied().unwrap_or(t.seq);
        if obligations.insert(obligation) && selected(t.time, since) {
            m.reruns += 1;
            m.reruns_failing += u32::from(t.failed());
        }
    }
    for t in p.trials.iter().filter(|t| {
        selected(t.time, since)
            && t.intent.activity == Some(Activity::Checking)
            && t.intent.condition == Condition::Together
    }) {
        let rerun = t.intent.rerun;
        let key = StepKey {
            rerun,
            cycle: if rerun {
                0
            } else {
                t.intent.cycle.map_or(0, std::num::NonZeroU32::get)
            },
            regime: t.intent.regime,
            cores: core_list(t.class.cores),
            outcome: t.outcome(),
        };
        *m.steps.entry(key).or_default() += 1;
    }
    m
}

pub(crate) fn render_checking(table: &mut Table, m: &CheckingMetrics) {
    table.section("Checking", &["metric", "count"]);
    row!(table, "cycles started", m.cycles_started);
    row!(table, "cycles ended", m.cycles_ended);
    row!(table, "full cycles", m.full_cycles);
    row!(table, "reruns", m.reruns);
    row!(table, "reruns failing first trial", m.reruns_failing);
    table.section(
        "Checking steps and together outcomes",
        &["cycle", "regime", "loaded cores", "outcome", "trials"],
    );
    for (key, count) in &m.steps {
        let cycle = if key.rerun {
            "rerun".to_owned()
        } else {
            key.cycle.to_string()
        };
        row!(
            table,
            cycle,
            key.regime,
            key.cores,
            outcome_name(key.outcome),
            count
        );
    }
}
