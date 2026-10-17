//! What each R7 chain derivation cost: which profile change re-derived it, whether its loaded set changed,
//! the trials it required and whether the cycle's chain still held its set at its last derivation. Computed
//! from `checking.chain` and the trials of the same cycle and step, without the tuner's evidence ledger: a
//! derivation that ran no trial before its chain moved on was covered by evidence it already had.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;

use jiff::Timestamp;
use serde::Serialize;
use togi_journal::{CheckingChain, CycleEvent, Outcome, Payload, Rederived, Seq, TrialId};
use togi_machine::{CcdId, CoreSet, Regime, WorkloadId};

use crate::project::{Projection, Trial, core_list};
use crate::r7::seq_list;
use crate::report::{hours, selected};
use crate::table::{Table, row};

/// How the tuner classified a derivation: the first at its chain position in the cycle's step, or a
/// re-derivation after a profile change that kept or changed the loaded set.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    First,
    Unchanged,
    Changed,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Unchanged => "unchanged",
            Self::Changed => "changed",
        }
    }
}

/// Where the profile change since the position's previous derivation moved a core: on the derivation's CCD,
/// or only on another.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Change {
    SameCcd,
    OtherCcd,
}

/// What became of a derivation: its set ran trials, its requirements were already met when the chain moved
/// on or its cycle ended, it was re-derived before running anything, the journal ends with it pending, or
/// it ended the chain.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Resolution {
    Ran,
    Covered,
    Superseded,
    Open,
    End,
}

impl Resolution {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ran => "ran",
            Self::Covered => "covered",
            Self::Superseded => "superseded",
            Self::Open => "open",
            Self::End => "end",
        }
    }
}

/// A trial of a derivation's loaded set in its cycle and step, before the position was derived again.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct ChainTrial<'a> {
    pub(crate) seq: Seq,
    pub(crate) trial: &'a TrialId,
    pub(crate) duration_s: u32,
    pub(crate) outcome: Option<Outcome>,
    pub(crate) crash: bool,
    /// Seconds from a crashed trial's last evidence to the start of the next boot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recovery_s: Option<f64>,
}

impl ChainTrial<'_> {
    fn failed(&self) -> bool {
        self.outcome == Some(Outcome::Failure)
    }
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Derivation<'a> {
    pub(crate) seq: Seq,
    #[serde(skip)]
    pub(crate) time: Timestamp,
    pub(crate) cycle: NonZeroU32,
    pub(crate) step: NonZeroU32,
    pub(crate) ccd: CcdId,
    pub(crate) workload: WorkloadId,
    pub(crate) part: NonZeroU32,
    pub(crate) groups: &'a [CoreSet],
    pub(crate) cores: CoreSet,
    pub(crate) sources: &'a [Seq],
    pub(crate) kind: Kind,
    /// The position's previous derivation in the cycle's step.
    pub(crate) previous: Option<Seq>,
    /// The cores whose offsets differ from the previous derivation's profile.
    pub(crate) moved: CoreSet,
    pub(crate) change: Option<Change>,
    /// A changed set that an earlier derivation of the step's chain on this CCD already required.
    pub(crate) repeated: bool,
    pub(crate) resolution: Resolution,
    /// The chain's last derivation of the step still holds this set at this position.
    pub(crate) held: bool,
    pub(crate) trials: Vec<ChainTrial<'a>>,
    /// Full-duration reruns of the set in the same window: rerun obligations, not chain work.
    pub(crate) reruns: Vec<ChainTrial<'a>>,
}

impl Derivation<'_> {
    /// A changed set that no earlier derivation of the step required: a load the old order never ran.
    fn new_load(&self) -> bool {
        self.kind == Kind::Changed && !self.repeated
    }
}

type Slot = (NonZeroU32, NonZeroU32, CcdId);
type TrialKey = (NonZeroU32, NonZeroU32, WorkloadId, CoreSet);

/// The chain each cycle step holds after its last derivation, by position: a derivation replaces its
/// position, and one that ends the chain also drops the positions after it.
fn final_chain(derivations: &[&CheckingChain]) -> Vec<(NonZeroU32, CoreSet)> {
    let mut nodes: Vec<(NonZeroU32, CoreSet)> = Vec::new();
    for d in derivations {
        match nodes.iter().position(|&(part, _)| part == d.part) {
            None => nodes.push((d.part, d.cores)),
            Some(at) if d.cores.is_empty() => {
                nodes.truncate(at);
                nodes.push((d.part, d.cores));
            }
            Some(at) => nodes[at] = (d.part, d.cores),
        }
    }
    nodes
}

fn chain_trial<'a>(p: &Projection<'a>, t: &Trial<'a>) -> ChainTrial<'a> {
    let crash = t.crash();
    ChainTrial {
        seq: t.seq,
        trial: &t.intent.trial,
        duration_s: t.seconds(),
        outcome: t.outcome(),
        crash,
        recovery_s: crash
            .then(|| p.next_boot.get(t.boot).and_then(|next| p.boots.get(next)))
            .flatten()
            .map(|boot| boot.start.duration_since(t.last_evidence).as_secs_f64()),
    }
}

/// Every `checking.chain` event at or after `since`, attributed against the whole journal so a
/// re-derivation after the cutoff still names what it re-derived.
pub(crate) fn attribute<'a>(p: &Projection<'a>, since: Option<Timestamp>) -> Vec<Derivation<'a>> {
    let mut chains: Vec<(Seq, Timestamp, &'a CheckingChain)> = Vec::new();
    let mut ended: HashMap<NonZeroU32, Seq> = HashMap::new();
    for event in p.events {
        match &event.payload {
            Payload::CheckingChain(chain) => chains.push((event.seq, event.time, chain)),
            Payload::CheckingCycle(cycle) if cycle.event == CycleEvent::End => {
                ended.entry(cycle.cycle).or_insert(event.seq);
            }
            _ => {}
        }
    }
    let mut trials: HashMap<TrialKey, Vec<&Trial<'a>>> = HashMap::new();
    for t in &p.trials {
        if let (Regime::R7, Some(cycle), Some(step)) =
            (t.intent.regime, t.intent.cycle, t.intent.step)
        {
            trials
                .entry((cycle, step, t.class.workload, t.class.cores))
                .or_default()
                .push(t);
        }
    }
    let mut slots: BTreeMap<Slot, Vec<usize>> = BTreeMap::new();
    for (at, &(_, _, chain)) in chains.iter().enumerate() {
        slots
            .entry((chain.cycle, chain.step, chain.ccd))
            .or_default()
            .push(at);
    }
    let mut out = Vec::new();
    for list in slots.values() {
        let in_slot: Vec<&CheckingChain> = list.iter().map(|&at| chains[at].2).collect();
        let last = final_chain(&in_slot);
        for (k, &at) in list.iter().enumerate() {
            let (seq, time, d) = chains[at];
            if !selected(time, since) {
                continue;
            }
            let earlier = &list[..k];
            let later = &list[k + 1..];
            let previous = earlier
                .iter()
                .rev()
                .map(|&j| chains[j])
                .find(|(_, _, old)| old.part == d.part);
            let until = later
                .iter()
                .map(|&j| chains[j])
                .find(|(_, _, next)| next.part <= d.part)
                .map(|(seq, _, _)| seq);
            let moved: CoreSet = previous.map_or_else(CoreSet::default, |(_, _, old)| {
                p.topology
                    .all()
                    .iter()
                    .filter(|&core| old.profile.get(core) != d.profile.get(core))
                    .collect()
            });
            let change = (!moved.is_empty()).then(|| {
                if moved.iter().any(|core| p.ccd_of(core) == Some(d.ccd)) {
                    Change::SameCcd
                } else {
                    Change::OtherCcd
                }
            });
            let kind = match d.rederived {
                None => Kind::First,
                Some(Rederived::Unchanged) => Kind::Unchanged,
                Some(Rederived::Changed) => Kind::Changed,
            };
            let repeated = kind == Kind::Changed
                && !d.cores.is_empty()
                && earlier.iter().any(|&j| chains[j].2.cores == d.cores);
            let in_window =
                |t: &&&Trial<'a>| t.seq > seq && until.is_none_or(|until| t.seq < until);
            let matching: Vec<&Trial<'a>> = if d.cores.is_empty() {
                Vec::new()
            } else {
                trials
                    .get(&(d.cycle, d.step, d.workload, d.cores))
                    .map(|found| found.iter().filter(in_window).copied().collect())
                    .unwrap_or_default()
            };
            let (reruns, work): (Vec<&Trial<'a>>, Vec<&Trial<'a>>) =
                matching.into_iter().partition(|t| t.intent.rerun);
            let resolution = if d.cores.is_empty() {
                Resolution::End
            } else if !work.is_empty() {
                Resolution::Ran
            } else {
                match later.first().map(|&j| chains[j].2) {
                    Some(next) if next.part > d.part => Resolution::Covered,
                    Some(_) => Resolution::Superseded,
                    None if ended.get(&d.cycle).is_some_and(|&end| end > seq) => {
                        Resolution::Covered
                    }
                    None => Resolution::Open,
                }
            };
            out.push(Derivation {
                seq,
                time,
                cycle: d.cycle,
                step: d.step,
                ccd: d.ccd,
                workload: d.workload,
                part: d.part,
                groups: &d.groups,
                cores: d.cores,
                sources: &d.source_seqs,
                kind,
                previous: previous.map(|(seq, _, _)| seq),
                moved,
                change,
                repeated,
                resolution,
                held: last.contains(&(d.part, d.cores)),
                trials: work.into_iter().map(|t| chain_trial(p, t)).collect(),
                reruns: reruns.into_iter().map(|t| chain_trial(p, t)).collect(),
            });
        }
    }
    out.sort_by_key(|d| d.seq);
    out
}

/// One JSON object a derivation, one a line, in journal order.
pub(crate) fn records(derivations: &[Derivation<'_>]) -> Result<String, serde_json::Error> {
    let mut text = String::new();
    for d in derivations {
        text.push_str(&serde_json::to_string(d)?);
        text.push('\n');
    }
    Ok(text)
}

#[derive(Default)]
struct Cost<'a> {
    derivations: u32,
    trials: u32,
    seconds: u64,
    failed: Vec<&'a TrialId>,
    failed_seconds: u64,
    recovery_s: f64,
}

impl<'a> Cost<'a> {
    fn add(&mut self, trials: &[ChainTrial<'a>]) {
        self.derivations += 1;
        for t in trials {
            self.trials += 1;
            self.seconds += u64::from(t.duration_s);
            if t.failed() {
                self.failed.push(t.trial);
                self.failed_seconds += u64::from(t.duration_s);
            }
            self.recovery_s += t.recovery_s.unwrap_or(0.0);
        }
    }

    /// `0`, or the count and the failed trials: `1: 0278`.
    fn failures(&self) -> String {
        if self.failed.is_empty() {
            return "0".to_owned();
        }
        let ids: Vec<String> = self.failed.iter().map(ToString::to_string).collect();
        format!("{}: {}", self.failed.len(), ids.join(","))
    }
}

/// The cores of each request group, highest request first: `00 > 01,02`.
fn group_list(groups: &[CoreSet]) -> String {
    groups
        .iter()
        .map(|&group| core_list(group))
        .collect::<Vec<_>>()
        .join(" > ")
}

#[expect(
    clippy::cast_precision_loss,
    reason = "trial seconds of a session are far below 2^52"
)]
fn hours_of(seconds: u64) -> String {
    hours(seconds as f64)
}

pub(crate) fn render(table: &mut Table, derivations: &[Derivation<'_>]) {
    table.section(
        "R7 chain derivations",
        &[
            "seq",
            "cycle",
            "step",
            "CCD",
            "workload",
            "part",
            "request groups",
            "loaded cores",
            "kind",
            "moved",
            "result",
            "held",
            "trials",
            "trial s",
            "sources",
        ],
    );
    for d in derivations {
        let sources = if d.sources.is_empty() {
            "offset fallback".to_owned()
        } else {
            seq_list(d.sources)
        };
        let moved = match d.change {
            None => "-".to_owned(),
            Some(Change::SameCcd) => core_list(d.moved),
            Some(Change::OtherCcd) => format!("{} (other CCD)", core_list(d.moved)),
        };
        let kind = if d.repeated {
            "changed, repeated"
        } else {
            d.kind.as_str()
        };
        row!(
            table,
            d.seq,
            d.cycle,
            d.step,
            d.ccd,
            d.workload,
            d.part,
            group_list(d.groups),
            core_list(d.cores),
            kind,
            moved,
            d.resolution.as_str(),
            if d.held { "yes" } else { "no" },
            d.trials.len(),
            d.trials.iter().map(|t| t.duration_s).sum::<u32>(),
            sources
        );
    }
    render_summary(table, derivations);
}

const COST_HEADER: [&str; 7] = [
    "derivations",
    "trials",
    "trial s",
    "hours",
    "failed s",
    "recovery s",
    "failures",
];

fn cost_row(table: &mut Table, labels: &[&str], cost: &Cost<'_>) {
    let mut cells: Vec<String> = labels.iter().map(|&label| label.to_owned()).collect();
    cells.extend([
        cost.derivations.to_string(),
        cost.trials.to_string(),
        cost.seconds.to_string(),
        hours_of(cost.seconds),
        cost.failed_seconds.to_string(),
        format!("{:.0}", cost.recovery_s),
        cost.failures(),
    ]);
    table.row(cells);
}

fn render_summary(table: &mut Table, derivations: &[Derivation<'_>]) {
    let mut by: BTreeMap<(Kind, Resolution), Cost<'_>> = BTreeMap::new();
    let mut other_ccd = Cost::default();
    let mut repeated = Cost::default();
    let mut new_loads = Cost::default();
    let mut released = Cost::default();
    let mut reruns = Cost::default();
    for d in derivations {
        by.entry((d.kind, d.resolution)).or_default().add(&d.trials);
        if d.kind == Kind::Unchanged && d.change == Some(Change::OtherCcd) {
            other_ccd.add(&d.trials);
        }
        if d.repeated {
            repeated.add(&d.trials);
        }
        if d.new_load() {
            new_loads.add(&d.trials);
        }
        if !d.held && !d.cores.is_empty() {
            released.add(&d.trials);
        }
        if !d.reruns.is_empty() {
            reruns.add(&d.reruns);
        }
    }
    let mut header = vec!["kind", "result"];
    header.extend(COST_HEADER);
    table.section("R7 chain cost by derivation", &header);
    if by.is_empty() {
        row!(table, "none");
    }
    for ((kind, resolution), cost) in &by {
        cost_row(table, &[kind.as_str(), resolution.as_str()], cost);
    }
    let mut header = vec!["field"];
    header.extend(COST_HEADER);
    table.section("R7 chain attribution", &header);
    cost_row(
        table,
        &["unchanged after a move on the other CCD"],
        &other_ccd,
    );
    cost_row(
        table,
        &["changed back to a set the step required before"],
        &repeated,
    );
    cost_row(
        table,
        &["changed to a set the step never required"],
        &new_loads,
    );
    cost_row(table, &["sets the step's chain no longer holds"], &released);
    cost_row(
        table,
        &["reruns of derived sets, not in the cost above"],
        &reruns,
    );
}
