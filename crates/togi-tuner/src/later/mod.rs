//! Later failures: once phase 2 has concluded and the profile has passed a full cycle since it
//! last changed, a core steps back only on its second failure within K cycle starts. The first is
//! held: it moves nothing, and the BIOS profile shows the step-back the tuner has not made. Every
//! input is kept by the fold; the decision functions only read.

mod bios;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use togi_journal::{Activity, Decision, Event, HuntSkipped, Seq, TunerDecision};
use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use crate::failures::Pending;
use crate::hunt::plan::park;
use crate::state::Run;
use crate::{Action, DecisionAction, DecisionPayload, Depth, Profile};

pub(crate) use bios::Confirmed;

/// K: two failures against one core pair when their cycles differ by less than this.
pub(crate) const WINDOW: u32 = 5;

/// A passed full cycle: the event that ended it and its number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FullCycle {
    pub(crate) seq: Seq,
    pub(crate) cycle: NonZeroU32,
}

/// A held failure: the cores it counts against at the offsets the hold found them at, and the
/// offsets the BIOS profile shows for them while the hold is valid. `ledger` is the id the
/// evidence ledger knows the failure by.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Strike {
    pub(crate) failure: Seq,
    pub(crate) ledger: Seq,
    pub(crate) hold: Seq,
    pub(crate) cycle: NonZeroU32,
    pub(crate) cores: PerCore<TargetOffset>,
    pub(crate) display: PerCore<TargetOffset>,
}

impl Strike {
    fn struck(&self) -> impl Iterator<Item = CoreId> + '_ {
        self.cores.iter().map(|(core, _)| core)
    }
}

/// The later-failure substate. A stored strike is always valid: the fold drops it when a core it
/// counts against moves or its window passes, so there is at most one per core and cycle of the
/// window. `held` keeps the ledger ids of held failures for the known-failure rule, after their
/// strikes are gone.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Later {
    confirmed: Option<Confirmed>,
    moved_since: PerCore<Option<Seq>>,
    strikes: BTreeMap<Seq, Strike>,
    by_core: BTreeMap<CoreId, BTreeSet<Seq>>,
    held: BTreeSet<Seq>,
    last_full: Option<FullCycle>,
}

impl Later {
    /// A reset returned a core to search: phase 2 starts over, with no confirmed profile and no
    /// hold.
    pub(crate) fn reset(&mut self) {
        self.confirmed = None;
        self.moved_since = PerCore::default();
        self.strikes.clear();
        self.by_core.clear();
        self.held.clear();
    }

    pub(crate) fn strikes(&self) -> impl Iterator<Item = &Strike> {
        self.strikes.values()
    }

    pub(crate) const fn confirmed(&self) -> Option<Confirmed> {
        self.confirmed
    }

    pub(crate) fn moved_since(&self, core: CoreId) -> Option<Seq> {
        self.moved_since.get(core).flatten()
    }

    /// Whether the evidence ledger's failure `ledger` was held: the known-failure rule never
    /// answers it, so its class is tested again.
    pub(crate) fn is_held(&self, ledger: Seq) -> bool {
        self.held.contains(&ledger)
    }

    fn insert(&mut self, strike: &Strike) {
        for core in strike.struck() {
            self.by_core.entry(core).or_default().insert(strike.hold);
        }
        self.held.insert(strike.ledger);
        self.strikes.insert(strike.hold, *strike);
    }

    fn remove(&mut self, hold: Seq) -> Option<Strike> {
        let strike = self.strikes.remove(&hold)?;
        for core in strike.struck() {
            if let Some(holds) = self.by_core.get_mut(&core) {
                holds.remove(&hold);
                if holds.is_empty() {
                    self.by_core.remove(&core);
                }
            }
        }
        Some(strike)
    }

    fn drop_core(&mut self, core: CoreId) {
        let holds = self.by_core.get(&core).cloned().unwrap_or_default();
        for hold in holds {
            self.remove(hold);
        }
    }

    fn holds_on(&self, cores: CoreSet) -> BTreeSet<Seq> {
        cores
            .iter()
            .filter_map(|core| self.by_core.get(&core))
            .flatten()
            .copied()
            .collect()
    }

    /// The oldest strike of another failure within the window that counts against one of `cores`.
    pub(crate) fn paired(
        &self,
        failure: Seq,
        cycle: NonZeroU32,
        cores: CoreSet,
    ) -> Option<&Strike> {
        self.holds_on(cores)
            .into_iter()
            .filter_map(|hold| self.strikes.get(&hold))
            .find(|strike| {
                strike.failure != failure
                    && cycle >= strike.cycle
                    && cycle.get() - strike.cycle.get() < WINDOW
            })
    }

    /// Consumes the strike an event cites last among at least two causes when it counts against
    /// one of `cores`: each hold pairs once.
    pub(crate) fn release(&mut self, cause: &[Seq], cores: CoreSet) -> Option<Strike> {
        let [_, .., last] = cause else {
            return None;
        };
        let hold = self.holds_on(cores).into_iter().find(|hold| {
            self.strikes
                .get(hold)
                .is_some_and(|strike| strike.failure == *last)
        })?;
        self.remove(hold)
    }

    fn cycle_start(&mut self, cycle: NonZeroU32) {
        let expired: Vec<Seq> = self
            .strikes
            .values()
            .filter(|strike| cycle.get().saturating_sub(strike.cycle.get()) >= WINDOW)
            .map(|strike| strike.hold)
            .collect();
        for hold in expired {
            self.remove(hold);
        }
    }

    fn full_cycle(&mut self, full: FullCycle, profile: Option<Profile>, settled: bool) {
        self.last_full = Some(full);
        let Some(profile) = profile else {
            return;
        };
        if settled {
            self.confirmed = Some(Confirmed {
                profile,
                seq: full.seq,
            });
            self.moved_since = PerCore::default();
        }
    }

    fn note_move(&mut self, core: CoreId, seq: Seq, offset: TargetOffset) {
        let Some(confirmed) = self.confirmed.and_then(|c| c.profile.get(core)) else {
            return;
        };
        if offset.shallower_than(confirmed) && self.moved_since(core).is_none() {
            self.moved_since.set(core, Some(seq));
        }
    }
}

/// A failure the later-failure rules may hold or pair, and the cycle it happened in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LaterFailure<'a> {
    pub(crate) seq: Seq,
    pub(crate) record: &'a Pending,
    pub(crate) cycle: NonZeroU32,
}

fn cores_text(cores: CoreSet) -> String {
    let names: Vec<String> = cores
        .iter()
        .map(|core| format!("{:02}", core.get()))
        .collect();
    names.join(", ")
}

fn deadline(cycle: NonZeroU32) -> u32 {
    cycle.get() + WINDOW - 1
}

fn pair_clause(strike: &Strike, failure: &LaterFailure<'_>) -> String {
    format!(
        "second failure within {WINDOW} cycles: #{} (cycle {}, held) and #{} (cycle {})",
        strike.failure, strike.cycle, failure.seq, failure.cycle
    )
}

impl Run {
    /// The latest passed full cycle when a first later failure is held: phase 2 concluded, that
    /// cycle ended after the conclusion and after the latest `profile.change`, and the checking
    /// profile is the decided one.
    pub(crate) fn later_gate(&self) -> Option<FullCycle> {
        let concluded = self.phases.concluded()?;
        let last = self.later.last_full?;
        let open = last.seq > concluded
            && self.checking.seq().is_none_or(|changed| last.seq > changed)
            && self.checking.profile() == Some(self.offsets());
        open.then_some(last)
    }

    /// The failure at `seq` when the later-failure rules apply to it: a live failure after the
    /// conclusion, not a located hunt's source, not a known-failure skip and not attributed at
    /// CO 0. An R7 failure some of
    /// whose cores are still owed an answer is found through its open record.
    pub(crate) fn later_failure(&self, seq: Seq) -> Option<LaterFailure<'_>> {
        let concluded = self.phases.concluded()?;
        if self.hunts.is_located(seq) {
            return None;
        }
        let record = self
            .failures
            .pending(seq)
            .or_else(|| self.r7.open.get(&seq).map(|open| &open.pending))?;
        let eligible = !record.carried
            && record.failure.known_failure.is_none()
            && seq > concluded
            && record.failure.offset != Some(TargetOffset::ZERO);
        eligible.then_some(LaterFailure {
            seq,
            record,
            cycle: record.cycle?,
        })
    }

    /// An ordinary checking step-back after the conclusion answered by the later-failure rules:
    /// a second failure against the core within the window steps it back citing both; a first
    /// one, once the gate is open, holds the core where the failure found it.
    pub(crate) fn later_decision(&self, action: Action) -> Action {
        match self.later_answer(&action) {
            Some(answer) => Action::Decide(answer),
            None => action,
        }
    }

    fn later_answer(&self, action: &Action) -> Option<DecisionAction> {
        let Action::Decide(DecisionAction {
            payload: DecisionPayload::TunerDecision(decision),
            cause,
        }) = action
        else {
            return None;
        };
        if decision.activity != Activity::Checking
            || decision.decision != Decision::Backoff
            || decision.to_offset == decision.from_offset
        {
            return None;
        }
        let failure = self.later_failure(*cause.first()?)?;
        let core = decision.core;
        let only: CoreSet = std::iter::once(core).collect();
        if let Some(strike) = self.later.paired(failure.seq, failure.cycle, only) {
            let mut paired = decision.clone();
            paired.reason = format!("{}; {}", paired.reason, pair_clause(strike, &failure));
            let mut cause = cause.clone();
            cause.push(strike.failure);
            return Some(DecisionAction::new(paired, cause));
        }
        let last = self.later_gate()?;
        let current = self.core(core);
        if failure.seq <= last.seq || failure.record.profile.get(core) != Some(current.offset) {
            return None;
        }
        let reason = format!(
            "holds at {}: failure #{} (cycle {}) is the first counting against core {:02} since the profile passed cycle {} (#{}); a second failure against it by cycle {} (K = {WINDOW}) steps it back",
            current.offset.get(),
            failure.seq,
            failure.cycle,
            core.get(),
            last.cycle,
            last.seq,
            deadline(failure.cycle),
        );
        Some(DecisionAction::new(
            TunerDecision {
                core,
                activity: Activity::Checking,
                decision: Decision::Hold,
                from_offset: current.offset,
                to_offset: current.offset,
                pass: current.pass,
                failure_point: self.failure_point(core),
                workloads: Vec::new(),
                reason,
            },
            cause.clone(),
        ))
    }

    /// The held failure the queued failure at `seq` is the second of, with the clause naming
    /// both: its hunt cites the held failure last and consumes the hold.
    pub(crate) fn later_hunt_pair(&self, seq: Seq) -> Option<(Seq, String)> {
        let failure = self.later_failure(seq)?;
        let strike = self
            .later
            .paired(seq, failure.cycle, failure.record.class.loaded())?;
        Some((strike.failure, pair_clause(strike, &failure)))
    }

    /// Holds the unattributed failure at the head of the hunt queue after the conclusion when no
    /// earlier held failure loads one of its cores and the gate is open: its hunt is skipped.
    pub(crate) fn later_hunt_hold(&self) -> Option<DecisionAction> {
        let seq = self.failures.queue().next()?;
        let failure = self.later_failure(seq)?;
        let cores = failure.record.class.loaded();
        if self.later.paired(seq, failure.cycle, cores).is_some() {
            return None;
        }
        let last = self.later_gate()?;
        if seq <= last.seq {
            return None;
        }
        let reason = format!(
            "held: first unattributed failure loading cores [{}] since the profile passed cycle {} (#{}); a second failure loading any of them by cycle {} (K = {WINDOW}) is hunted",
            cores_text(cores),
            last.cycle,
            last.seq,
            deadline(failure.cycle),
        );
        Some(DecisionAction::new(
            HuntSkipped {
                failure: seq,
                reason,
            },
            vec![seq],
        ))
    }

    /// The strike a `hold` decision records, read from the state it was decided on: the core at
    /// its current offset, shown at the target of the step-back the hold replaced.
    pub(crate) fn hold_strike(&self, event: &Event, decision: &TunerDecision) -> Option<Strike> {
        if decision.decision != Decision::Hold {
            return None;
        }
        let failure = *event.cause.first()?;
        let record = self
            .failures
            .pending(failure)
            .or_else(|| self.r7.open.get(&failure).map(|open| &open.pending))?;
        let mut display = PerCore::default();
        if let Some(Action::Decide(DecisionAction {
            payload: DecisionPayload::TunerDecision(ordinary),
            cause,
        })) = self.ordinary_decision()
        {
            if cause.first() == Some(&failure)
                && ordinary.core == decision.core
                && ordinary.decision == Decision::Backoff
                && ordinary.from_offset == decision.from_offset
                && ordinary.to_offset.shallower_than(ordinary.from_offset)
            {
                display.set(decision.core, ordinary.to_offset);
            }
        }
        Some(Strike {
            failure,
            ledger: record.ledger,
            hold: event.seq,
            cycle: record.cycle?,
            cores: std::iter::once((decision.core, decision.from_offset)).collect(),
            display,
        })
    }

    /// A decision folded: a real checking step-back consumes the strike it pairs with, a move
    /// drops the other strikes of its core, and a hold records its strike. `hold` was read from
    /// the state before the decision applied.
    pub(crate) fn later_decided(
        &mut self,
        event: &Event,
        decision: &TunerDecision,
        before: TargetOffset,
        hold: Option<&Strike>,
    ) {
        if decision.activity == Activity::Checking
            && decision.decision == Decision::Backoff
            && decision.to_offset != decision.from_offset
        {
            let only: CoreSet = std::iter::once(decision.core).collect();
            self.later.release(&event.cause, only);
        }
        self.later_moved(decision.core, event.seq, before);
        if let Some(strike) = hold {
            self.later.insert(strike);
        }
    }

    /// The core's offset may have changed from `before`: a move drops its strikes and marks it
    /// for the BIOS profile.
    pub(crate) fn later_moved(&mut self, core: CoreId, seq: Seq, before: TargetOffset) {
        let offset = self.core(core).offset;
        if offset != before {
            self.later.drop_core(core);
        }
        self.later.note_move(core, seq, offset);
    }

    /// A `hunt.skipped` holds its failure when the failure counts after the conclusion and its
    /// profile reaches no recorded constraint, which an ordinary skip always does. The strike
    /// counts against the loaded cores and shows every candidate one count shallower.
    pub(crate) fn later_hunt_skipped(&mut self, event: &Event, skipped: &HuntSkipped) {
        let Some(failure) = self.later_failure(skipped.failure) else {
            return;
        };
        if self.constraints.reaches(&failure.record.profile) != Ok(None) {
            return;
        }
        let cores = failure
            .record
            .class
            .loaded()
            .iter()
            .map(|core| (core, self.core(core).offset))
            .collect();
        let display = park(&failure.record.profile, self.hunts.passed_newest_first())
            .candidates
            .iter()
            .map(|core| (core, self.core(core).offset))
            .filter(|&(_, offset)| offset != TargetOffset::ZERO)
            .map(|(core, offset)| (core, offset.shallower_by(1)))
            .collect();
        let strike = Strike {
            failure: failure.seq,
            ledger: failure.record.ledger,
            hold: event.seq,
            cycle: failure.cycle,
            cores,
            display,
        };
        self.later.insert(&strike);
    }

    pub(crate) fn later_cycle_start(&mut self, cycle: NonZeroU32) {
        self.later.cycle_start(cycle);
    }

    /// A passed full cycle ended: it is the latest for the gate, and from phase 1's end on it
    /// confirms the checking profile.
    pub(crate) fn later_full_cycle(&mut self, seq: Seq, cycle: NonZeroU32) {
        let settled = self.phases.phase1_end().is_some();
        let profile = self.checking.profile();
        self.later
            .full_cycle(FullCycle { seq, cycle }, profile, settled);
    }
}
