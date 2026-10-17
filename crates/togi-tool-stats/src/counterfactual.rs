//! Counterfactual accounting of R7 chain re-derivations (#896): for each predeclared candidate rule, the
//! derivations whose trials it would not have run, an upper bound on the seconds that saves while every
//! passed cycle's final chains stay covered, the passes that cover them, the observed failures it would lose
//! or delay, and the decisions it leaves unidentified. The journal is folded into the tuner, whose evidence
//! ledger counts coverage and whose request order derives a candidate's own chain; no other tuner is run or
//! simulated, and removing trials from a journal is not what another tuner would have done.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::{NonZeroU16, NonZeroU32};

use jiff::Timestamp;
use serde::Serialize;
use togi_journal::{CycleEvent, Event, Outcome, Payload, Seq, TrialId};
use togi_machine::{CcdId, CoreSet, PerCore, Regime, TargetOffset, WorkloadId};
use togi_requests::VOLTS_PER_COUNT;
use togi_tuner::{ChainOrder, ClassKey, FoldError, LoadKey, Secs, State, Window};

use crate::chains::{self, Derivation, Kind};
use crate::error::Error;
use crate::journal::Journal;
use crate::project::Projection;
use crate::report::hours;
use crate::table::{Table, row};

/// How far below a CCD's highest request the tie-aware candidate still counts a core as tied: one offset
/// count, the request change a one-count backoff makes.
const TIE_MARGIN_V: f64 = VOLTS_PER_COUNT;

/// The predeclared candidate rules, in the order the brief lists them.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Candidate {
    /// Only the last derivation of each chain position in a cycle runs: a re-derivation that a later one
    /// at the same or an earlier position replaces runs nothing.
    Coalesce,
    /// An earlier R7 step's re-derivations run only once the cycle's last R7 step has begun.
    Defer,
    /// A position keeps the set it holds while the cores that set idles stay within one offset count of
    /// the CCD's highest request.
    Tie,
}

const CANDIDATES: [Candidate; 3] = [Candidate::Coalesce, Candidate::Defer, Candidate::Tie];

impl Candidate {
    fn as_str(self) -> &'static str {
        match self {
            Self::Coalesce => "coalesce",
            Self::Defer => "defer",
            Self::Tie => "tie",
        }
    }
}

/// Why a candidate's outcome for a derivation cannot be told from the journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum Unresolved {
    /// The candidate skipped this failure, so its profile and every later decision differ.
    AfterFailure { failure: Seq },
    /// A tuner decision, hunt, combination, deepening round or core phase before this derivation cites
    /// `source`, a trial the candidate skipped, so that decision's offsets and every later decision may
    /// differ.
    AfterDecision { decision: Seq, source: Seq },
    /// The order the derivation read cites the measurement of a trial the candidate skipped.
    Source { source: Seq },
    /// The candidate's chain holds another set before this position, or the candidate would derive
    /// another set here.
    Diverged,
    /// The tuner, folded to this derivation, does not give its recorded request groups.
    Order,
}

/// What a candidate does with one derivation's trials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub(crate) enum Verdict {
    Kept,
    Skipped,
    Unresolved(Unresolved),
}

/// A chain position as a candidate holds it: the set the tuner derived, an older set the candidate keeps
/// instead, or a set it cannot tell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Position {
    Same(CoreSet),
    Kept { old: CoreSet, new: CoreSet },
    Unknown { actual: CoreSet },
}

impl Position {
    fn actual(self) -> CoreSet {
        match self {
            Self::Same(cores) | Self::Kept { new: cores, .. } | Self::Unknown { actual: cores } => {
                cores
            }
        }
    }
}

type Slot = (NonZeroU32, NonZeroU32, CcdId);

/// How a cycle's final chains stand under a candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CycleStatus {
    /// The cycle passed and every final chain class is counted.
    Covered,
    /// The candidate skipped a failure, or a trial a tuner decision cites, in or before the cycle.
    Diverged,
    /// A final chain position is one the candidate cannot tell.
    Unidentified,
    /// The cycle ended without passing: a core reset ended it.
    Abandoned,
    /// The journal ends with the cycle open.
    Open,
}

/// A trial of a derivation, as the accounting needs it.
#[derive(Clone, Copy, Debug)]
struct Run<'a> {
    end: Option<Seq>,
    trial: &'a TrialId,
    seconds: u32,
    outcome: Option<Outcome>,
}

/// One final chain class of a covered cycle under a candidate: the passes it needs at the cycle's ending
/// profile, the valid passes the candidate keeps that cover it, the skipped or unresolved passes it must still
/// run and the passes no trial in the journal supplies.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Coverage {
    pub(crate) step: NonZeroU32,
    pub(crate) ccd: CcdId,
    pub(crate) workload: WorkloadId,
    pub(crate) cores: CoreSet,
    /// The set the tuner's chain held instead, when the candidate keeps an older one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) instead_of: Option<CoreSet>,
    pub(crate) duration_s: u32,
    pub(crate) needed: u32,
    pub(crate) witnesses: Vec<Seq>,
    pub(crate) rerun: Vec<Seq>,
    pub(crate) unobserved: usize,
}

/// A failure a candidate skipped: `delayed` when its set is still in the candidate's final chain, so the
/// candidate tests it later, `lost` otherwise.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Failure<'a> {
    pub(crate) seq: Seq,
    pub(crate) trial: &'a TrialId,
    pub(crate) derivation: Seq,
    pub(crate) cycle: NonZeroU32,
    pub(crate) cores: CoreSet,
    pub(crate) delayed: bool,
}

/// One record of the structured output.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub(crate) enum Record<'a> {
    /// A derivation a candidate does not keep as the tuner ran it.
    Derivation {
        candidate: Candidate,
        seq: Seq,
        cycle: NonZeroU32,
        step: NonZeroU32,
        ccd: CcdId,
        part: NonZeroU32,
        cores: CoreSet,
        #[serde(flatten)]
        verdict: Verdict,
        trials: usize,
        trial_s: u64,
    },
    Failure {
        candidate: Candidate,
        #[serde(flatten)]
        failure: Failure<'a>,
    },
    /// A cycle that held chain derivations, under a candidate.
    Cycle {
        candidate: Candidate,
        cycle: NonZeroU32,
        status: CycleStatus,
        /// The ending profile the coverage is counted at, for a covered cycle.
        #[serde(skip_serializing_if = "Option::is_none")]
        profile: Option<PerCore<TargetOffset>>,
        skipped_s: u64,
        required_s: u64,
        unresolved_s: u64,
        coverage: Vec<Coverage>,
    },
    Summary(Summary),
}

/// A candidate's totals over the journal. Every chain trial's seconds are kept, skipped in a covered cycle
/// or unresolved: `kept_s + skipped_s + unresolved_s` is `trial_s`. Of the skipped seconds, `required_s`
/// rerun skipped passes the final chains still need or pay for passes the journal never observed;
/// `avoidable_s` is the rest, an upper bound.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Summary {
    pub(crate) candidate: Option<Candidate>,
    pub(crate) derivations: u32,
    pub(crate) skipped: u32,
    pub(crate) unresolved: u32,
    pub(crate) trial_s: u64,
    pub(crate) kept_s: u64,
    pub(crate) skipped_s: u64,
    pub(crate) unresolved_s: u64,
    pub(crate) required_s: u64,
    pub(crate) avoidable_s: i128,
    pub(crate) unobserved: usize,
    pub(crate) lost: u32,
    pub(crate) delayed: u32,
    pub(crate) cycles_covered: u32,
    pub(crate) cycles_unresolved: u32,
    /// Seconds from the session's first event to its first clean cycle's end, or to the journal's last
    /// event when no cycle was clean.
    pub(crate) conclusion_s: u64,
    pub(crate) concluded: bool,
}

/// What one candidate has decided so far, folded derivation by derivation.
#[derive(Default)]
struct Ledger<'a> {
    chains: BTreeMap<Slot, Vec<(NonZeroU32, Position)>>,
    skipped: HashSet<Seq>,
    /// The trial ends of the derivations the candidate leaves unresolved: whether it runs them is unknown,
    /// so they witness nothing.
    uncertain: HashSet<Seq>,
    /// Where the candidate stops following the journal, the first skipped failure by its end or the first
    /// tuner decision citing a skipped trial, and the reason every later derivation is unresolved.
    diverged: Option<(Seq, Unresolved)>,
    records: Vec<Record<'a>>,
    cycles: BTreeMap<NonZeroU32, CycleTally>,
    summary: Summary,
}

#[derive(Default)]
struct CycleTally {
    skipped_s: u64,
    unresolved_s: u64,
}

/// The facts of one derivation the candidates decide on.
struct Facts<'d, 'a> {
    d: &'d Derivation<'a>,
    runs: Vec<Run<'a>>,
    /// A later derivation at the same or an earlier position of the cycle's step on the CCD replaces it.
    replaced: bool,
    /// The cycle's last R7 step had begun when it was derived.
    last_step_begun: bool,
    last_r7_step: Option<NonZeroU32>,
}

impl Facts<'_, '_> {
    fn seconds(&self) -> u64 {
        self.runs.iter().map(|run| u64::from(run.seconds)).sum()
    }

    fn load(&self) -> CoreSet {
        self.d.groups.iter().flat_map(CoreSet::iter).collect()
    }
}

/// The set a chain derivation from `load` leaves once the order's top group idles: empty when fewer than
/// two cores remain, as the tuner ends a chain.
pub(crate) fn next_set(load: CoreSet, order: &ChainOrder) -> CoreSet {
    let top = order.groups.first().copied().unwrap_or_default();
    let rest: CoreSet = load.iter().filter(|&core| !top.contains(core)).collect();
    if rest.len() < 2 {
        CoreSet::default()
    } else {
        rest
    }
}

/// Whether the order idling `load - held` instead of its top group differs from the tuner's only within
/// [`TIE_MARGIN_V`]: every core the held set idles requests within the margin of the highest request.
pub(crate) fn within_tie(load: CoreSet, held: CoreSet, order: &ChainOrder) -> bool {
    let idled: CoreSet = load.iter().filter(|&core| !held.contains(core)).collect();
    let top = order.values.iter().map(|(_, value)| value).reduce(f64::max);
    !held.is_empty()
        && held.iter().all(|core| load.contains(core))
        && !idled.is_empty()
        && top.is_some_and(|top| {
            idled.iter().all(|core| {
                order
                    .values
                    .get(core)
                    .is_some_and(|value| value >= top - TIE_MARGIN_V)
            })
        })
}

fn record_position(chain: &mut Vec<(NonZeroU32, Position)>, part: NonZeroU32, position: Position) {
    match chain.iter().position(|&(at, _)| at == part) {
        None => chain.push((part, position)),
        Some(at) if position.actual().is_empty() => {
            chain.truncate(at);
            chain.push((part, position));
        }
        Some(at) => chain[at] = (part, position),
    }
}

impl<'a> Ledger<'a> {
    fn new(candidate: Candidate) -> Self {
        Self {
            summary: Summary {
                candidate: Some(candidate),
                ..Summary::default()
            },
            ..Self::default()
        }
    }

    /// The verdict of `f` under `candidate` and the position the candidate's chain holds after it, with the
    /// tuner folded through the event before the derivation.
    fn decide(
        &self,
        candidate: Candidate,
        f: &Facts<'_, 'a>,
        state: &State,
    ) -> (Verdict, Position) {
        let d = f.d;
        let unknown = Position::Unknown { actual: d.cores };
        if let Some((_, reason)) = self.diverged {
            return (Verdict::Unresolved(reason), unknown);
        }
        if let Some(&source) = d.sources.iter().find(|seq| self.skipped.contains(seq)) {
            return (Verdict::Unresolved(Unresolved::Source { source }), unknown);
        }
        let chain = self.chains.get(&(d.cycle, d.step, d.ccd));
        let at = |part: NonZeroU32| {
            chain.and_then(|chain| chain.iter().find(|&&(at, _)| at == part).map(|&(_, p)| p))
        };
        let predecessor =
            NonZeroU32::new(d.part.get() - 1).map_or(Some(Position::Same(f.load())), at);
        let same = Position::Same(d.cores);
        match predecessor {
            None | Some(Position::Unknown { .. }) => {
                return (Verdict::Unresolved(Unresolved::Diverged), unknown);
            }
            Some(Position::Kept { old, .. }) => {
                let Some(order) = state.chain_order(LoadKey::new(d.workload, old), d.ccd) else {
                    return (Verdict::Unresolved(Unresolved::Diverged), unknown);
                };
                if let Some(&source) = order.sources.iter().find(|seq| self.skipped.contains(seq)) {
                    return (Verdict::Unresolved(Unresolved::Source { source }), unknown);
                }
                return if next_set(old, &order) == d.cores {
                    (Verdict::Kept, same)
                } else {
                    (Verdict::Unresolved(Unresolved::Diverged), unknown)
                };
            }
            Some(Position::Same(_)) => {}
        }
        let rederived = d.kind != Kind::First;
        match candidate {
            Candidate::Coalesce if rederived && f.replaced => (Verdict::Skipped, same),
            Candidate::Defer
                if rederived
                    && !f.last_step_begun
                    && f.last_r7_step.is_some_and(|last| d.step < last) =>
            {
                (Verdict::Skipped, same)
            }
            Candidate::Coalesce | Candidate::Defer => (Verdict::Kept, same),
            Candidate::Tie => {
                let (cf, previous) = match at(d.part) {
                    None => return (Verdict::Kept, same),
                    Some(Position::Unknown { .. }) => {
                        return (Verdict::Unresolved(Unresolved::Diverged), unknown);
                    }
                    Some(Position::Same(cores)) => (cores, cores),
                    Some(Position::Kept { old, new }) => (old, new),
                };
                if cf == d.cores {
                    return (Verdict::Kept, same);
                }
                let load = f.load();
                let Some(order) = state.chain_order(LoadKey::new(d.workload, load), d.ccd) else {
                    return (Verdict::Unresolved(Unresolved::Order), unknown);
                };
                if order.groups != d.groups {
                    return (Verdict::Unresolved(Unresolved::Order), unknown);
                }
                if !d.cores.is_empty() && within_tie(load, cf, &order) {
                    (
                        Verdict::Skipped,
                        Position::Kept {
                            old: cf,
                            new: d.cores,
                        },
                    )
                } else if cf == previous || d.cores != previous {
                    (Verdict::Kept, same)
                } else {
                    (Verdict::Unresolved(Unresolved::Diverged), unknown)
                }
            }
        }
    }

    fn derive(&mut self, candidate: Candidate, f: &Facts<'_, 'a>, state: &State) {
        let (verdict, position) = self.decide(candidate, f, state);
        let d = f.d;
        record_position(
            self.chains.entry((d.cycle, d.step, d.ccd)).or_default(),
            d.part,
            position,
        );
        let seconds = f.seconds();
        let tally = self.cycles.entry(d.cycle).or_default();
        self.summary.derivations += 1;
        self.summary.trial_s += seconds;
        match verdict {
            Verdict::Kept => self.summary.kept_s += seconds,
            Verdict::Skipped => {
                self.summary.skipped += 1;
                tally.skipped_s += seconds;
                for run in &f.runs {
                    let Some(end) = run.end else { continue };
                    self.skipped.insert(end);
                    if run.outcome == Some(Outcome::Failure) {
                        let delayed = match candidate {
                            Candidate::Coalesce | Candidate::Defer => d.held,
                            Candidate::Tie => false,
                        };
                        if delayed {
                            self.summary.delayed += 1;
                        } else {
                            self.summary.lost += 1;
                        }
                        self.diverged
                            .get_or_insert((end, Unresolved::AfterFailure { failure: end }));
                        self.records.push(Record::Failure {
                            candidate,
                            failure: Failure {
                                seq: end,
                                trial: run.trial,
                                derivation: d.seq,
                                cycle: d.cycle,
                                cores: d.cores,
                                delayed,
                            },
                        });
                    }
                }
            }
            Verdict::Unresolved(_) => {
                self.summary.unresolved += 1;
                tally.unresolved_s += seconds;
                self.uncertain
                    .extend(f.runs.iter().filter_map(|run| run.end));
            }
        }
        if verdict != Verdict::Kept {
            self.records.push(Record::Derivation {
                candidate,
                seq: d.seq,
                cycle: d.cycle,
                step: d.step,
                ccd: d.ccd,
                part: d.part,
                cores: d.cores,
                verdict,
                trials: f.runs.len(),
                trial_s: seconds,
            });
        }
    }

    /// Marks the candidate diverged at `event` when it is a tuner decision, hunt, combination, deepening
    /// round or core phase citing a trial the candidate skipped: without that trial the decision, its
    /// offsets and every later decision may differ. A located hunt cites the passes that covered the
    /// loaded cores, which a candidate may have skipped.
    fn cite(&mut self, event: &Event) {
        let decides = matches!(
            event.payload,
            Payload::TunerDecision(_)
                | Payload::HuntStart(_)
                | Payload::HuntGroup(_)
                | Payload::HuntEnd(_)
                | Payload::HuntSkipped(_)
                | Payload::Combination(_)
                | Payload::DeepeningRound(_)
                | Payload::CorePhase(_)
        );
        if self.diverged.is_some() || !decides {
            return;
        }
        if let Some(&source) = event.cause.iter().find(|seq| self.skipped.contains(seq)) {
            self.diverged = Some((
                event.seq,
                Unresolved::AfterDecision {
                    decision: event.seq,
                    source,
                },
            ));
        }
    }

    /// Closes `cycle` with the tuner folded through the event before its end, or through the journal's last
    /// event when it is still open.
    fn close(
        &mut self,
        candidate: Candidate,
        cycle: &CycleFacts,
        ended: Option<(Seq, bool)>,
        state: &State,
    ) {
        let Some(tally) = self.cycles.remove(&cycle.number) else {
            return;
        };
        let status = match ended {
            None => CycleStatus::Open,
            Some((_, false)) => CycleStatus::Abandoned,
            Some((end, true)) if self.diverged.is_some_and(|(at, _)| at < end) => {
                CycleStatus::Diverged
            }
            Some(_) => CycleStatus::Covered,
        };
        let covered = (status == CycleStatus::Covered)
            .then(|| self.cover(cycle, state))
            .flatten();
        let (status, profile, coverage) = match covered {
            Some((profile, coverage)) => (CycleStatus::Covered, Some(profile), coverage),
            None if status == CycleStatus::Covered => (CycleStatus::Unidentified, None, Vec::new()),
            None => (status, None, Vec::new()),
        };
        let (skipped_s, required_s, unresolved_s) = if status == CycleStatus::Covered {
            let required: u64 = coverage
                .iter()
                .map(|c| {
                    let passes = c.rerun.len() + c.unobserved;
                    passes as u64 * u64::from(c.duration_s)
                })
                .sum();
            self.summary.cycles_covered += 1;
            self.summary.skipped_s += tally.skipped_s;
            self.summary.required_s += required;
            self.summary.unobserved += coverage.iter().map(|c| c.unobserved).sum::<usize>();
            (tally.skipped_s, required, tally.unresolved_s)
        } else {
            self.summary.cycles_unresolved += 1;
            (0, 0, tally.unresolved_s + tally.skipped_s)
        };
        self.summary.unresolved_s += unresolved_s;
        self.records.push(Record::Cycle {
            candidate,
            cycle: cycle.number,
            status,
            profile,
            skipped_s,
            required_s,
            unresolved_s,
            coverage,
        });
    }

    /// Every final chain class of the cycle's R7 steps, as the candidate holds the chains, counted by the
    /// tuner's evidence ledger at the ending profile in the cycle's window. `None` when a position is one the
    /// candidate cannot tell or the tuner has no ending profile.
    fn cover(
        &self,
        cycle: &CycleFacts,
        state: &State,
    ) -> Option<(PerCore<TargetOffset>, Vec<Coverage>)> {
        let profile = state.project().checking.profile?;
        let evidence = state.evidence()?;
        let plan = state.cycle_plan();
        let mut needed: BTreeMap<ClassKey, Coverage> = BTreeMap::new();
        // The tuner multiplies a part's base quota by one more than the earlier parts of its workload
        // that hold the same set (`checking::schedule`). The candidate's chain may hold other sets, so
        // its quota multiplies the same base by its own count of earlier parts holding its set.
        let mut tuner_sets: HashMap<(WorkloadId, CoreSet), u32> = HashMap::new();
        let mut held_sets: HashMap<(WorkloadId, CoreSet), u32> = HashMap::new();
        for (index, step) in plan.steps.iter().enumerate() {
            let number = NonZeroU32::new(u32::try_from(index + 1).ok()?)?;
            let mut held_parts = Vec::with_capacity(step.parts.len());
            for part in &step.parts {
                if step.regime != Regime::R7 || !part.partial {
                    held_parts.push(part.cores);
                    continue;
                }
                let ccd = part.ccd?;
                let held = self
                    .chains
                    .get(&(cycle.number, number, ccd))
                    .and_then(|chain| {
                        chain
                            .iter()
                            .map(|&(_, position)| position)
                            .find(|position| position.actual() == part.cores)
                    })
                    .unwrap_or(Position::Same(part.cores));
                let (cores, instead_of) = match held {
                    Position::Same(cores) => (cores, None),
                    Position::Kept { old, new } => (old, Some(new)),
                    Position::Unknown { .. } => return None,
                };
                held_parts.push(cores);
                let earlier = |sets: &HashMap<(WorkloadId, CoreSet), u32>, cores| {
                    NonZeroU32::MIN
                        .saturating_add(sets.get(&(step.workload, cores)).copied().unwrap_or(0))
                };
                let tuner = earlier(&tuner_sets, part.cores);
                let own = earlier(&held_sets, cores);
                for (count, secs) in [(part.short, part.short_s), (part.long, part.long_s)] {
                    let Some(secs) = NonZeroU32::new(secs).filter(|_| count > 0) else {
                        continue;
                    };
                    let class = ClassKey::new(step.workload, cores, Secs::new(secs));
                    let coverage = needed.entry(class).or_insert_with(|| Coverage {
                        step: number,
                        ccd,
                        workload: step.workload,
                        cores,
                        instead_of,
                        duration_s: secs.get(),
                        needed: 0,
                        witnesses: Vec::new(),
                        rerun: Vec::new(),
                        unobserved: 0,
                    });
                    // A later part's quota already includes the earlier parts' passes of its class, and
                    // the tuner checks each quota on its own, so the class needs the largest.
                    let quota = (count / tuner).saturating_mul(own.get());
                    coverage.needed = coverage.needed.max(quota);
                }
            }
            for (part, cores) in step.parts.iter().zip(held_parts) {
                for (sets, cores) in [(&mut tuner_sets, part.cores), (&mut held_sets, cores)] {
                    let seen = sets.entry((step.workload, cores)).or_insert(0);
                    *seen = seen.saturating_add(1);
                }
            }
        }
        let window = Window::Live {
            since: Some(cycle.start),
        };
        let mut out = Vec::with_capacity(needed.len());
        for (class, mut coverage) in needed {
            let valid = evidence
                .count(class, &profile, window, NonZeroU16::MAX)
                .ok()?
                .witnesses;
            let need = usize::try_from(coverage.needed).ok()?;
            let (kept, skipped): (Vec<Seq>, Vec<Seq>) = valid
                .into_iter()
                .partition(|seq| !self.skipped.contains(seq) && !self.uncertain.contains(seq));
            coverage.witnesses = kept.into_iter().take(need).collect();
            let missing = need - coverage.witnesses.len();
            coverage.rerun = skipped.into_iter().take(missing).collect();
            coverage.unobserved = missing - coverage.rerun.len();
            out.push(coverage);
        }
        Some((profile.offsets(), out))
    }
}

/// A cycle's start and the last R7 step of its schedule.
struct CycleFacts {
    number: NonZeroU32,
    start: Seq,
    last_r7_step: Option<NonZeroU32>,
}

/// The accounting of a journal: the per-candidate records, one JSON object a line, and their summaries.
pub(crate) struct Accounting<'a> {
    pub(crate) records: Vec<Record<'a>>,
    pub(crate) summaries: Vec<Summary>,
}

/// The counterfactual accounting of a journal's R7 chain re-derivations: the tables to print and the
/// records, one JSON object a line.
#[derive(Debug)]
pub struct Counterfactual {
    pub text: String,
    pub records: String,
}

/// Accounts `journal`'s R7 chain re-derivations under each predeclared candidate (`docs/benchmarking.md`,
/// Counterfactual re-derivation accounting).
///
/// # Errors
///
/// When the tuner cannot fold the journal or a record cannot be encoded.
pub fn counterfactual(journal: &Journal) -> Result<Counterfactual, Error> {
    let projection = Projection::new(journal);
    let accounting = account(&projection).map_err(Error::Fold)?;
    Ok(Counterfactual {
        text: render(&accounting),
        records: records(&accounting).map_err(Error::Records)?,
    })
}

/// Folds `p`'s journal into the tuner and accounts every chain derivation under each candidate.
pub(crate) fn account<'a>(p: &Projection<'a>) -> Result<Accounting<'a>, FoldError> {
    let derivations = chains::attribute(p, None);
    let ends: HashMap<Seq, Seq> = p
        .trials
        .iter()
        .filter_map(|t| t.end.map(|end| (t.seq, end.seq)))
        .collect();
    let replaced = replaced(&derivations);
    let by_seq: HashMap<Seq, &Derivation<'a>> = derivations.iter().map(|d| (d.seq, d)).collect();
    let mut ledgers: Vec<(Candidate, Ledger<'a>)> =
        CANDIDATES.iter().map(|&c| (c, Ledger::new(c))).collect();
    let mut cycles: BTreeMap<NonZeroU32, CycleFacts> = BTreeMap::new();
    let mut begun: HashSet<NonZeroU32> = HashSet::new();
    let mut state = State::new();
    let mut conclusion: Option<Timestamp> = None;
    for event in p.events {
        for (_, ledger) in &mut ledgers {
            ledger.cite(event);
        }
        if let Payload::CheckingCycle(cycle) = &event.payload
            && cycle.event == CycleEvent::Start
        {
            let last_r7_step = cycle
                .steps
                .iter()
                .rposition(|&regime| regime == Regime::R7)
                .and_then(|at| u32::try_from(at + 1).ok())
                .and_then(NonZeroU32::new);
            cycles.insert(
                cycle.cycle,
                CycleFacts {
                    number: cycle.cycle,
                    start: event.seq,
                    last_r7_step,
                },
            );
        } else if let Payload::CheckingStep(step) = &event.payload {
            if cycles
                .get(&step.cycle)
                .and_then(|c| c.last_r7_step)
                .is_some_and(|last| last == step.step)
            {
                begun.insert(step.cycle);
            }
        } else if let Payload::CheckingChain(_) = &event.payload {
            if let Some(d) = by_seq.get(&event.seq) {
                let facts = Facts {
                    d,
                    runs: d
                        .trials
                        .iter()
                        .map(|t| Run {
                            end: ends.get(&t.seq).copied(),
                            trial: t.trial,
                            seconds: t.duration_s,
                            outcome: t.outcome,
                        })
                        .collect(),
                    replaced: replaced.contains(&d.seq),
                    last_step_begun: begun.contains(&d.cycle),
                    last_r7_step: cycles.get(&d.cycle).and_then(|c| c.last_r7_step),
                };
                for (candidate, ledger) in &mut ledgers {
                    ledger.derive(*candidate, &facts, &state);
                }
            }
        } else if let Payload::CheckingCycle(cycle) = &event.payload
            && cycle.event == CycleEvent::End
        {
            if let Some(facts) = cycles.remove(&cycle.cycle) {
                for (candidate, ledger) in &mut ledgers {
                    ledger.close(*candidate, &facts, Some((event.seq, cycle.passed)), &state);
                }
            }
        }
        state.fold(event)?;
        if conclusion.is_none() && is_cycle_end(event) && state.project().clean_cycles >= 1 {
            conclusion = Some(event.time);
        }
    }
    for facts in cycles.values() {
        for (candidate, ledger) in &mut ledgers {
            ledger.close(*candidate, facts, None, &state);
        }
    }
    let conclusion_s = p
        .events
        .first()
        .zip(conclusion.or_else(|| p.events.last().map(|event| event.time)))
        .map_or(0, |(first, end)| {
            u64::try_from(end.duration_since(first.time).as_secs()).unwrap_or(0)
        });
    let mut records = Vec::new();
    let mut summaries = Vec::new();
    for (_, mut ledger) in ledgers {
        let s = &mut ledger.summary;
        s.avoidable_s = i128::from(s.skipped_s) - i128::from(s.required_s);
        s.conclusion_s = conclusion_s;
        s.concluded = conclusion.is_some();
        records.append(&mut ledger.records);
        records.push(Record::Summary(ledger.summary.clone()));
        summaries.push(ledger.summary);
    }
    Ok(Accounting { records, summaries })
}

fn is_cycle_end(event: &Event) -> bool {
    matches!(&event.payload, Payload::CheckingCycle(cycle) if cycle.event == CycleEvent::End)
}

/// The derivations a later derivation at the same or an earlier position of the same cycle step and CCD
/// replaces.
fn replaced(derivations: &[Derivation<'_>]) -> HashSet<Seq> {
    let mut slots: BTreeMap<Slot, Vec<&Derivation<'_>>> = BTreeMap::new();
    for d in derivations {
        slots.entry((d.cycle, d.step, d.ccd)).or_default().push(d);
    }
    let mut out = HashSet::new();
    for list in slots.values() {
        for (at, d) in list.iter().enumerate() {
            if list[at + 1..].iter().any(|later| later.part <= d.part) {
                out.insert(d.seq);
            }
        }
    }
    out
}

/// One JSON object a record, one a line.
pub(crate) fn records(accounting: &Accounting<'_>) -> Result<String, serde_json::Error> {
    let mut text = String::new();
    for record in &accounting.records {
        text.push_str(&serde_json::to_string(record)?);
        text.push('\n');
    }
    Ok(text)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "seconds of a session are far below 2^52"
)]
fn share(seconds: i128, of: u64) -> String {
    if of == 0 {
        return "-".to_owned();
    }
    format!("{:.3}", seconds as f64 / of as f64)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "seconds of a session are far below 2^52"
)]
fn signed_hours(seconds: i128) -> String {
    hours(seconds as f64)
}

pub(crate) fn render(accounting: &Accounting<'_>) -> String {
    let mut table = Table::default();
    table.line(
        "Counterfactual accounting of R7 chain re-derivations: upper bounds from removing trials from this journal, not what another tuner would have done.",
    );
    if let Some(s) = accounting.summaries.first() {
        table.line(&format!(
            "{} derivations, {} s of chain trials; {} s to {}.",
            s.derivations,
            s.trial_s,
            s.conclusion_s,
            if s.concluded {
                "the first clean cycle"
            } else {
                "the journal's end, no clean cycle"
            }
        ));
    }
    table.section(
        "Candidates",
        &[
            "candidate",
            "skipped",
            "unresolved",
            "kept s",
            "skipped s",
            "required s",
            "avoidable s",
            "avoidable h",
            "of conclusion",
            "unresolved s",
            "unobserved",
            "lost",
            "delayed",
            "cycles covered",
            "cycles unresolved",
        ],
    );
    for s in &accounting.summaries {
        row!(
            table,
            s.candidate.map_or("-", Candidate::as_str),
            s.skipped,
            s.unresolved,
            s.kept_s,
            s.skipped_s,
            s.required_s,
            s.avoidable_s,
            signed_hours(s.avoidable_s),
            share(s.avoidable_s, s.conclusion_s),
            s.unresolved_s,
            s.unobserved,
            s.lost,
            s.delayed,
            s.cycles_covered,
            s.cycles_unresolved
        );
    }
    let failures: Vec<_> = accounting
        .records
        .iter()
        .filter_map(|record| {
            if let Record::Failure { candidate, failure } = record {
                Some((candidate, failure))
            } else {
                None
            }
        })
        .collect();
    if !failures.is_empty() {
        table.section(
            "Skipped failures",
            &[
                "candidate",
                "trial",
                "end seq",
                "derivation",
                "cycle",
                "effect",
            ],
        );
        for (candidate, f) in failures {
            row!(
                table,
                candidate.as_str(),
                f.trial,
                f.seq,
                f.derivation,
                f.cycle,
                if f.delayed { "delayed" } else { "lost" }
            );
        }
    }
    table.finish()
}
