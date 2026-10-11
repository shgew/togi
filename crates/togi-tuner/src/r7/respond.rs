use std::fmt;

use togi_journal::{Activity, Decision, HuntOutcome, Seq, TunerDecision};
use togi_machine::{CcdId, Condition, CoreId, CoreSet, PerCore, TargetOffset};
use togi_requests::groups;

use super::Applied;
use super::escalation::Escalation;
use super::failed::FailedTrial;
use super::order::lowest_ranked;
use super::reason::{Basis, Note, R7Reason, ids};
use super::target::{Ask, OwnRequest, Target, backoff_counts, backoff_move};
use crate::hunt::state::{Located, LocatedEnd};
use crate::ledger::carried_clause;
use crate::r7::failed::OpenFailure;
use crate::state::Run;
use crate::zero::{AtZero, Diagnostic};
use crate::{Action, DecisionAction, Depth, Profile, TrialPlan};

/// A failed multi-core R7 trial as attribution reads it now: once its all-zero rerun passed,
/// neither its named nor its stalled core confines it any more.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Failed<'a> {
    pub(crate) record: &'a OpenFailure,
    pub(crate) trial: &'a FailedTrial,
    pub(crate) named: Option<CoreId>,
    pub(crate) stalled: Option<CoreId>,
    pub(crate) rerun_end: Option<Seq>,
}

impl Failed<'_> {
    fn profile(&self) -> &Profile {
        &self.record.pending.profile
    }

    fn loaded(&self) -> CoreSet {
        self.record.pending.class.loaded()
    }
}

/// What a failed multi-core R7 trial asks of the tuner.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum R7Response {
    Backoff {
        decision: DecisionAction,
        why: R7Reason,
    },
    DeadEnd(DecisionAction),
    ZeroRerun(Box<TrialPlan>),
    ReadRanking,
}

pub(crate) type LocatedLink<'a> = (&'a Located, &'a LocatedEnd);

fn located_clause(located: &Located, end: &LocatedEnd) -> (String, Vec<Seq>) {
    let mut seqs = vec![end.seq];
    let hunt = located.hunt;
    let text = match (&end.named, end.failure) {
        (Some(named), Some(failure)) => {
            seqs.push(failure);
            format!(
                "hunt {hunt} ended loaded after failure #{failure} named loaded core {:02}, which also answers failure #{}",
                named.core.get(),
                located.source
            )
        }
        (None, Some(failure)) => {
            seqs.push(failure);
            format!(
                "hunt {hunt} kept the failure on the loaded cores after failure #{failure} with every unloaded core at CO 0"
            )
        }
        (Some(_) | None, None) => {
            format!("hunt {hunt} kept the failure on the loaded cores")
        }
    };
    (text, seqs)
}

impl R7Response {
    pub(crate) fn sourced(mut self, source: Seq) -> Self {
        match &mut self {
            Self::Backoff { decision, .. } | Self::DeadEnd(decision) => {
                decision.cause.retain(|&seq| seq != source);
                decision.cause.insert(0, source);
            }
            Self::ZeroRerun(_) | Self::ReadRanking => {}
        }
        self
    }

    pub(crate) fn into_action(self) -> Action {
        match self {
            Self::Backoff { decision, .. } | Self::DeadEnd(decision) => Action::Decide(decision),
            Self::ZeroRerun(plan) => Action::RunTrial(*plan),
            Self::ReadRanking => Action::ReadRanking,
        }
    }
}

/// Why a failure is located on the unloaded cores before it is charged to the loaded ones.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LocateReason {
    pub(crate) rerun_passed: Option<Seq>,
    pub(crate) escalation: Option<Escalation>,
}

impl LocateReason {
    /// The events the located hunt cites besides the failure.
    pub(crate) fn cause(&self) -> Vec<Seq> {
        let mut cause: Vec<Seq> = self.rerun_passed.into_iter().collect();
        if let Some(Escalation::Backoffs { decisions, .. }) = &self.escalation {
            cause.extend(decisions.iter().copied());
        }
        cause
    }
}

impl fmt::Display for LocateReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.rerun_passed {
            Some(end) => write!(
                f,
                "the rerun with every core at CO 0 passed (#{end}), so it names no core and is located on the unloaded cores before it is charged to the loaded cores"
            )?,
            None => f.write_str(
                "no core is named, so the failure is located on the unloaded cores before it is charged to the loaded cores",
            )?,
        }
        if let Some(escalation) = &self.escalation {
            write!(f, "; {escalation}")?;
        }
        Ok(())
    }
}

/// Where a failed multi-core R7 trial goes: answered now, or kept for a located hunt.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum R7Route {
    Respond(Box<R7Response>),
    Locate { failure: Seq, reason: LocateReason },
}

/// The core group a failure charges, and what the order said on the way.
struct Charge {
    group: CoreSet,
    sources: Vec<Seq>,
    notes: Vec<Note>,
    rail: f64,
    stepped: bool,
    named: bool,
}

impl Charge {
    fn named(group: CoreSet, sources: Vec<Seq>) -> Self {
        Self {
            group,
            sources,
            notes: Vec::new(),
            rail: 0.0,
            stepped: false,
            named: true,
        }
    }
}

impl Run {
    pub(crate) fn failed<'a>(&'a self, record: &'a OpenFailure) -> Failed<'a> {
        let trial = &record.r7;
        let rerun_end = match self.zero.get(record.seq) {
            Some(&Diagnostic::Passed { end }) => Some(end),
            _ => None,
        };
        let confined = rerun_end.is_none();
        Failed {
            record,
            trial,
            named: record.pending.failure.core.filter(|_| confined),
            stalled: trial.stalled.filter(|_| confined),
            rerun_end,
        }
    }

    fn ccd_of(&self, core: CoreId) -> Option<CcdId> {
        self.topo.ccd_of(core)
    }

    fn at_zero_in(profile: &Profile, core: CoreId) -> bool {
        profile.at(core) == TargetOffset::ZERO
    }

    /// Whether a named failure counts against its named core. A core named at CO 0 on a CCD with
    /// no loaded core has no top group of its own, so its failure counts as unattributed.
    pub(crate) fn named_culprit(&self, view: &Failed<'_>) -> bool {
        let Some(named) = view.named else {
            return false;
        };
        !Self::at_zero_in(view.profile(), named)
            || view
                .loaded()
                .iter()
                .any(|core| self.ccd_of(core) == self.ccd_of(named))
    }

    fn loaded_movable(&self, view: &Failed<'_>, ccd: Option<CcdId>) -> bool {
        view.loaded()
            .iter()
            .any(|core| self.ccd_of(core) == ccd && !Self::at_zero_in(view.profile(), core))
    }

    /// The cores a failure counts against: its named core, or the top groups of the affected
    /// CCDs. When another affected CCD still has a movable loaded core, a CCD whose loaded cores
    /// were all at CO 0 in that trial is not affected.
    pub(crate) fn failure_targets(&self, view: &Failed<'_>) -> Vec<CoreId> {
        if self.named_culprit(view) {
            return view.named.into_iter().collect();
        }
        let mut top: Vec<CoreId> = view.trial.top().iter().collect();
        if let Some(stalled) = view.stalled {
            let ccd = self.ccd_of(stalled);
            top.retain(|&core| self.ccd_of(core) == ccd);
        }
        let movable: Vec<CoreId> = top
            .iter()
            .copied()
            .filter(|&core| self.loaded_movable(view, self.ccd_of(core)))
            .collect();
        if movable.is_empty() { top } else { movable }
    }

    fn shallower_than_failed(&self, view: &Failed<'_>, core: CoreId) -> bool {
        self.core(core)
            .offset
            .shallower_than(view.profile().at(core))
    }

    /// Whether every core a decision against `id` could move already sits shallower than in the
    /// failed trial. A named core at CO 0 is never answered here: whether it dead-ends depends on
    /// its trial's request order.
    fn answered(&self, view: &Failed<'_>, id: CoreId) -> bool {
        if self.named_culprit(view) {
            return self.shallower_than_failed(view, id);
        }
        let ccd = self.ccd_of(id);
        let mut movable = false;
        for core in view.loaded() {
            if self.ccd_of(core) != ccd || Self::at_zero_in(view.profile(), core) {
                continue;
            }
            if !self.shallower_than_failed(view, core) {
                return false;
            }
            movable = true;
        }
        movable
    }

    /// The unloaded cores off CO 0 in the failed profile.
    fn locate_candidates(&self, record: &OpenFailure) -> CoreSet {
        self.topo
            .all()
            .iter()
            .filter(|&core| {
                !record.pending.class.loaded().contains(core)
                    && !Self::at_zero_in(&record.pending.profile, core)
            })
            .collect()
    }

    /// Why a multi-core R7 failure is located before it is charged, or none when it is charged
    /// now: a live unattributed one whose load left an unloaded core off CO 0 and that escalates,
    /// and, carried or naming a core at CO 0, one whose loaded cores were all at CO 0.
    pub(crate) fn locatable(&self, view: &Failed<'_>) -> Option<LocateReason> {
        let record = view.record;
        if !record.pending.class.multi_r7()
            || (!record.pending.carried
                && record.pending.failure.condition == Some(Condition::Parked))
            || self.locate_candidates(record).is_empty()
        {
            return None;
        }
        let live = !record.pending.carried
            && record.pending.failure.condition == Some(Condition::Together);
        let rerun_passed = view.rerun_end;
        let loaded_at_zero = view
            .loaded()
            .iter()
            .all(|core| Self::at_zero_in(view.profile(), core));
        let affected: Vec<CcdId> = self
            .failure_targets(view)
            .iter()
            .filter_map(|&core| self.ccd_of(core))
            .collect();
        let escalation = view.trial.backoffs.escalation(
            &self.topo,
            record.pending.class.load(),
            view.profile(),
            &affected,
        );
        if loaded_at_zero {
            if view
                .named
                .is_some_and(|named| !Self::at_zero_in(view.profile(), named))
            {
                return None;
            }
            return Some(LocateReason {
                rerun_passed,
                escalation: Some(escalation.unwrap_or(Escalation::LoadedAtZero)),
            });
        }
        if !live || self.named_culprit(view) {
            return None;
        }
        escalation.map(|escalation| LocateReason {
            rerun_passed,
            escalation: Some(escalation),
        })
    }

    /// Whether a failure is located before it is charged, for a located hunt to start.
    pub(crate) fn eligible_for_locate(&self, record: &OpenFailure) -> bool {
        self.locatable(&self.failed(record)).is_some()
    }

    fn recorded_first_group(view: &Failed<'_>, part: CoreSet) -> CoreSet {
        view.trial
            .recorded_top
            .iter()
            .filter(|&core| part.contains(core))
            .collect()
    }

    /// The request groups of the loaded cores on `ccd`, highest first: the trial's recorded top
    /// requesters of the CCD lead when it recorded any.
    fn ccd_groups(view: &Failed<'_>, ccd: CcdId) -> Vec<CoreSet> {
        let Some(order) = view.trial.order.ccd(ccd) else {
            return Vec::new();
        };
        let first = Self::recorded_first_group(view, order.loaded());
        if first.is_empty() {
            return order.groups().to_vec();
        }
        let rest: PerCore<f64> = order
            .loaded()
            .iter()
            .filter(|&core| !first.contains(core))
            .filter_map(|core| view.trial.order.value(core).map(|value| (core, value)))
            .collect();
        let mut out = vec![first];
        out.extend(
            groups(&rest)
                .into_iter()
                .map(|group| group.into_iter().collect::<CoreSet>()),
        );
        out
    }

    fn charge(&self, view: &Failed<'_>, id: CoreId) -> Charge {
        let named = self.named_culprit(view);
        if named {
            if !Self::at_zero_in(view.profile(), id) {
                return Charge::named(std::iter::once(id).collect(), Vec::new());
            }
            if view.trial.top().contains(id) {
                let sources = if view.trial.recorded_top.is_empty() {
                    view.trial.order.sources().to_vec()
                } else {
                    Vec::new()
                };
                return Charge::named(std::iter::once(id).collect(), sources);
            }
        }
        let Some(ccd) = self.ccd_of(id) else {
            return Charge::named(CoreSet::default(), Vec::new());
        };
        let rail = view
            .trial
            .order
            .ccd(ccd)
            .map_or(0.0, super::order::CcdOrder::rail);
        let mut charge = Charge {
            group: CoreSet::default(),
            sources: view.trial.order.sources().to_vec(),
            notes: Vec::new(),
            rail,
            stepped: false,
            named: false,
        };
        if named {
            charge.notes.push(Note::NamedZeroNotTop { core: id, ccd });
        } else if let Some(core) = view
            .record
            .pending
            .failure
            .core
            .filter(|_| view.named.is_some())
        {
            if let Some(named_ccd) = self.ccd_of(core) {
                charge.notes.push(Note::NamedOnEmptyCcd {
                    named: core,
                    named_ccd,
                    loaded_ccd: ccd,
                });
            }
        }
        let groups = Self::ccd_groups(view, ccd);
        for (at, group) in groups.iter().enumerate() {
            let movable: CoreSet = group
                .iter()
                .filter(|&core| !Self::at_zero_in(view.profile(), core))
                .collect();
            if movable.is_empty() {
                continue;
            }
            if at > 0 {
                charge.stepped = true;
                charge.notes.push(Note::SteppedDown {
                    ccd,
                    skipped: groups[..at].to_vec(),
                    group: *group,
                });
            }
            charge.group = movable;
            return charge;
        }
        charge
    }

    fn failed_at_zero_head(&self, view: &Failed<'_>, id: CoreId, charge: &Charge) -> String {
        let ccd = self.ccd_of(id).map_or(0, |ccd| ccd.0);
        if charge.named {
            let basis = if !view.trial.recorded_top.is_empty() {
                "its trial's recorded top requesters".to_owned()
            } else if charge.sources.is_empty() {
                "offset order (no request telemetry)".to_owned()
            } else {
                let sources: Vec<String> = charge.sources.iter().map(ToString::to_string).collect();
                format!("request measurements [{}]", sources.join(" "))
            };
            return format!(
                "core {:02} failed at CO 0 as a top requester of CCD {ccd} by {basis}",
                id.get()
            );
        }
        if !self.named_culprit(view) {
            let subject = match view.record.pending.failure.core {
                Some(named) if view.named.is_some() => format!(
                    "R7 failure naming core {:02} at CO 0 on CCD {}, which had no loaded core,",
                    named.get(),
                    self.ccd_of(named).map_or(0, |ccd| ccd.0)
                ),
                _ => "unattributed R7 failure".to_owned(),
            };
            let loaded: CoreSet = view
                .loaded()
                .iter()
                .filter(|&core| self.ccd_of(core) == self.ccd_of(id))
                .collect();
            return format!(
                "{subject} counts against CCD {ccd}'s top group, and every loaded core of that CCD {} is at CO 0",
                ids(loaded)
            );
        }
        format!("core {:02} failed at CO 0", id.get())
    }

    /// The response a failure requires against target `id`, none when the core it would move
    /// already sits shallower than in the failed trial: the failure then needs no move, and
    /// stays pending in case that core returns to its failing offset.
    fn core_response(
        &self,
        view: &Failed<'_>,
        id: CoreId,
        link: Option<LocatedLink<'_>>,
    ) -> Option<R7Response> {
        if self.answered(view, id) {
            return None;
        }
        let record = view.record;
        let mut charge = self.charge(view, id);
        let rerun = view.rerun_end;
        let live = !record.pending.carried
            && record.pending.failure.condition == Some(Condition::Together);
        let mut located_seqs: Vec<Seq> = Vec::new();
        let mut clause: Option<String> = None;
        if let Some((located, end)) = link {
            let (text, seqs) = located_clause(located, end);
            charge.notes.push(Note::Located(text.clone()));
            clause = Some(text);
            located_seqs = seqs;
        } else if live && !self.named_culprit(view) && !self.locate_candidates(record).is_empty() {
            charge.notes.push(Note::Ordinal {
                n: view.trial.backoffs.failures() + 1,
                after: super::escalation::ESCALATE_AFTER,
            });
        }
        if let Some(end) = rerun {
            charge.notes.push(Note::RerunPassed {
                failure: record.seq,
                end,
            });
        }
        let mut cause = vec![record.seq];
        cause.extend(located_seqs);
        cause.extend(rerun);
        cause.extend(charge.sources.iter().copied());
        if charge.group.is_empty() || (charge.named && Self::at_zero_in(view.profile(), id)) {
            let mut head = self.failed_at_zero_head(view, id, &charge);
            if let Some(text) = &clause {
                head.push_str(", and ");
                head.push_str(text);
            }
            if link.is_some_and(|(_, end)| end.all_zero && end.failure.is_some()) {
                return Some(R7Response::DeadEnd(crate::zero::dead_end(
                    id, &head, None, cause,
                )));
            }
            return match self.at_zero(record.seq, &record.pending, id, &head, cause) {
                AtZero::DeadEnd(decision) => Some(R7Response::DeadEnd(*decision)),
                AtZero::Rerun(plan) => Some(R7Response::ZeroRerun(plan)),
                AtZero::Released => None,
            };
        }
        if charge.group.len() > 1 && self.ranking.is_none() {
            return Some(R7Response::ReadRanking);
        }
        let chosen = match &self.ranking {
            Some(ranking) => lowest_ranked(charge.group, &ranking.ranking)?,
            None => charge.group.first()?,
        };
        if self.shallower_than_failed(view, chosen) {
            return None;
        }
        if !charge.named {
            cause.extend(self.ranking.as_ref().map(|ranking| ranking.seq));
        }
        Some(self.backoff(view, chosen, cause, charge))
    }

    fn backoff(
        &self,
        view: &Failed<'_>,
        id: CoreId,
        mut cause: Vec<Seq>,
        charge: Charge,
    ) -> R7Response {
        let record = view.record;
        let failing = view.trial.failing_request(id);
        let applied = view.profile().at(id);
        let n = self.config.needed;
        let backend = self
            .evidence
            .active_backend(record.pending.class.workload().backend());
        let mut target: Option<Target> = None;
        let own = match failing.volts {
            Some(request) => {
                let failing_top = charge.rail;
                let ask = Ask {
                    topology: &self.topo,
                    evidence: &self.evidence,
                    backend,
                    n,
                };
                target = if charge.named {
                    let mhz = self
                        .ccd_of(id)
                        .and_then(|ccd| view.trial.clocks.get(&ccd).copied());
                    self.r7
                        .targets
                        .named(ask, record.pending.class.workload(), id, mhz)
                } else {
                    self.r7
                        .targets
                        .unattributed(ask, record.pending.class.load(), id, failing_top)
                };
                OwnRequest::Measured(request)
            }
            None => OwnRequest::OffsetProxy,
        };
        let rail = (charge.stepped && failing.volts.is_some()).then_some(charge.rail);
        let counts = backoff_counts(own, target.as_ref(), rail);
        let offset = applied.get();
        let basis = match (failing.volts, &target) {
            (None, _) if failing.sources.is_empty() => Basis::NoTelemetry { offset },
            (None, _) => Basis::Uncovered {
                offset,
                sources: failing.sources.clone(),
            },
            (Some(request), None) => Basis::NoPass { request },
            (Some(request), Some(target)) if request >= target.volts => Basis::Met {
                request,
                target: target.volts,
            },
            (Some(request), Some(target)) => Basis::Toward {
                request,
                target: target.volts,
            },
        };
        cause.extend(failing.sources.iter().copied());
        if let Some(target) = &target {
            cause.extend(target.cited.iter().copied());
        }
        let mut seen: Vec<Seq> = Vec::new();
        cause.retain(|seq| {
            let fresh = !seen.contains(seq);
            seen.push(*seq);
            fresh
        });
        let why = R7Reason {
            failure: record.seq,
            core: id,
            basis,
            counts,
            notes: charge.notes,
            rail,
        };
        let core = self.core(id);
        let moved = backoff_move(applied, self.failure_point(id), counts);
        let pass = core
            .pass
            .filter(|&pass| pass.shallower_than(moved.failure_point));
        let reason = format!("{why}{}", carried_clause(&self.evidence, &cause));
        let decision = DecisionAction::new(
            TunerDecision {
                core: id,
                activity: Activity::Checking,
                decision: Decision::Backoff,
                from_offset: core.offset,
                to_offset: moved.to,
                pass,
                failure_point: Some(moved.failure_point),
                workloads: Vec::new(),
                reason,
            },
            cause,
        );
        R7Response::Backoff { decision, why }
    }

    /// The route of one multi-core R7 failure whose cores `handled` already answered.
    fn r7_route(&self, record: &OpenFailure) -> Option<R7Route> {
        let link = match self.hunts.located.get(&record.seq) {
            None => None,
            Some(located) => match &located.end {
                Some(end) if end.result == HuntOutcome::Loaded => Some((located, end)),
                Some(_) | None => return None,
            },
        };
        if let Some((_, end)) = link {
            if let Some(named) = &end.named {
                if record.handled.contains(named.core) {
                    return None;
                }
                let view = self.failed(&named.failure);
                return self
                    .core_response(&view, named.core, link)
                    .map(|response| R7Route::Respond(Box::new(response.sourced(record.seq))));
            }
        }
        let view = self.failed(record);
        let unhandled: Vec<CoreId> = self
            .failure_targets(&view)
            .into_iter()
            .filter(|&core| !record.handled.contains(core))
            .collect();
        if unhandled.is_empty() {
            return None;
        }
        if link.is_none() {
            if let Some(reason) = self.locatable(&view) {
                let required = unhandled
                    .iter()
                    .find_map(|&core| self.core_response(&view, core, None))?;
                return Some(match required {
                    R7Response::ReadRanking => R7Route::Respond(Box::new(R7Response::ReadRanking)),
                    _ => R7Route::Locate {
                        failure: record.seq,
                        reason,
                    },
                });
            }
        }
        unhandled
            .iter()
            .find_map(|&core| self.core_response(&view, core, link))
            .map(|response| R7Route::Respond(Box::new(response)))
    }

    /// The first response an open multi-core R7 failure requires, in failure order. A failure
    /// kept for a located hunt waits; one whose moved core is already shallower stays pending.
    pub(crate) fn r7_decision(&self) -> Option<R7Response> {
        self.r7
            .open
            .values()
            .find_map(|record| match self.r7_route(record)? {
                R7Route::Respond(response) => Some(*response),
                R7Route::Locate { .. } => None,
            })
    }

    /// The first open failure waiting for a located hunt.
    pub(crate) fn locate_due(&self) -> Option<R7Route> {
        self.r7
            .open
            .values()
            .find_map(|record| match self.r7_route(record)? {
                route @ R7Route::Locate { .. } => Some(route),
                R7Route::Respond(_) => None,
            })
    }
}
