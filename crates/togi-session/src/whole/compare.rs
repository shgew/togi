//! The semantic comparison behind the cutpoint sweeps: what a resumed journal must share with the uninterrupted one.
//!
//! A resumed journal numbers its events differently. Sequence names retain first-mention aliasing and anchor
//! each citation to its target's kind and identity-hidden policy payload in the whole compared journal. Only in
//! an exposure anchor, its source boot is hidden too: a resumed trial legitimately runs on a new boot. Direct
//! exposure comparisons still compare that boot exactly, along with every other nonidentity field. Raw-fact
//! targets contribute only their kind: trial intents carry task start times and trial ends carry interruption
//! evidence that legitimately changes across a resume. Missing targets are explicitly marked outside-journal.
//! This is a bounded anchor, not recursive graph equality: targets with the same kind and hidden payload can
//! still agree. Night identities share their plan sequence's anchor; trial identities retain first-mention names.
//! All other rendered values, including counts, profiles, durations, boots and times, compare exactly.
//!
//! The types that cite an identity, directly or through a field, are taken apart by exhaustive destructuring,
//! so a new field fails to compile until this module says how it compares. A type that cites no identity is
//! compared whole, field by field, through its `Debug` form: a new field of one compares exactly.

use std::collections::HashMap;

use togi_journal::{
    ApplyCause, CcdCap, CcdEdge, CellEvidence, CellWitness, DayApplied, DayEvent, DayEventId,
    DayFreeze, DayHold, DayInterval, DayStep, DayVeto, Edge, ExposureObserved, FreezeSource,
    HoldChange, HoldReason, NightId, NightPlan, NightQualification, NightReproduction,
    OriginalSource, Payload, Provenance, QualificationState, Resolution, RetreatCause, Seq,
    StepCauses, StepKind, TrialId, UnavailableBackend,
};
use togi_tuner::policy::{
    CapProjection, DayProjection, EventProjection, FirmwareValidity, HoldProjection,
    NightProjection, Projection, QualificationProjection, ReproductionProjection, UnitProjection,
    VetoProjection,
};

/// How identities are named.
#[derive(Default)]
pub(super) struct Names<'events> {
    numbered: bool,
    events: HashMap<Seq, &'events togi_journal::Event>,
    seqs: HashMap<Seq, String>,
    trials: HashMap<TrialId, usize>,
}

impl<'events> Names<'events> {
    /// Names every identity alike: what a value looks like without the identities it cites.
    pub(super) fn hidden() -> Self {
        Self::default()
    }

    /// Names identities by first mention, anchoring sequences in the whole journal, not just a compared suffix.
    pub(super) fn numbered(events: &'events [togi_journal::Event]) -> Self {
        Self {
            numbered: true,
            events: events.iter().map(|event| (event.seq, event)).collect(),
            seqs: HashMap::new(),
            trials: HashMap::new(),
        }
    }

    fn seq(&mut self, seq: Seq) -> String {
        if !self.numbered {
            return "#".to_owned();
        }
        let next = self.seqs.len();
        self.seqs
            .entry(seq)
            .or_insert_with(|| {
                let anchor = match self.events.get(&seq) {
                    Some(event) => {
                        let kind = event.kind().as_str();
                        match Self::hidden().rendered_payload(&event.payload, true) {
                            Some(shape) => format!("{kind} {shape}"),
                            None => kind.to_owned(),
                        }
                    }
                    None => "outside-journal".to_owned(),
                };
                format!("#{next} ({anchor})")
            })
            .clone()
    }

    fn opt_seq(&mut self, seq: Option<Seq>) -> String {
        match seq {
            Some(seq) => format!("Some({})", self.seq(seq)),
            None => "None".to_owned(),
        }
    }

    /// The names of `seqs`: how many and which of them.
    fn seqs(&mut self, seqs: &[Seq]) -> String {
        let names: Vec<_> = seqs.iter().map(|&seq| self.seq(seq)).collect();
        format!("[{}]", names.join(", "))
    }

    fn trial(&mut self, trial: &TrialId) -> String {
        if !self.numbered {
            return "trial".to_owned();
        }
        let next = self.trials.len();
        format!("trial{}", self.trials.entry(trial.clone()).or_insert(next))
    }

    /// A night is the sequence of its plan: it shares names with the sequences that cite that plan.
    fn night(&mut self, night: NightId) -> String {
        let NightId(seq) = night;
        format!("night{}", self.seq(seq))
    }

    fn opt_night(&mut self, night: Option<NightId>) -> String {
        match night {
            Some(night) => format!("Some({})", self.night(night)),
            None => "None".to_owned(),
        }
    }

    fn edge(&mut self, edge: &Edge) -> String {
        match edge {
            Edge::Unknown {} => "unknown".to_owned(),
            Edge::Observed { offset, witness } => {
                format!("observed {offset:?} {}", self.seq(*witness))
            }
            Edge::Censored { limit, witnesses } => {
                format!("censored {limit:?} {}", self.seqs(witnesses))
            }
        }
    }

    fn event_id(&mut self, id: &DayEventId) -> String {
        match id {
            DayEventId::Boot { boot } => format!("boot {boot:?}"),
            DayEventId::Record { session, seq } => {
                format!("record {session:?} {}", self.seq(*seq))
            }
            DayEventId::Requalification { night } => {
                format!("requalification {}", self.night(*night))
            }
        }
    }

    fn opt_event_id(&mut self, id: Option<&DayEventId>) -> String {
        match id {
            Some(id) => format!("Some({})", self.event_id(id)),
            None => "None".to_owned(),
        }
    }

    fn provenance(&mut self, provenance: &Provenance) -> String {
        let Provenance { session, seq } = provenance;
        format!("{session:?} {}", self.seq(*seq))
    }

    fn resolution(&mut self, resolution: &Resolution) -> String {
        match resolution {
            Resolution::Unresolved {} => "unresolved".to_owned(),
            Resolution::Resolved { retreat } => format!("resolved {}", self.opt_seq(*retreat)),
        }
    }

    fn witnesses(&mut self, witnesses: &[CellWitness]) -> String {
        let cells: Vec<_> = witnesses
            .iter()
            .map(|witness| {
                let CellWitness { cell, evidence } = witness;
                match evidence {
                    CellEvidence::Fresh { exposure } => {
                        format!("{cell:?} fresh {}", self.seq(*exposure))
                    }
                    CellEvidence::Reused { exposure } => {
                        format!("{cell:?} reused {}", self.seq(*exposure))
                    }
                }
            })
            .collect();
        format!("[{}]", cells.join(", "))
    }

    fn qualification_state(&mut self, state: &QualificationState) -> String {
        match state {
            QualificationState::Start { outstanding } => format!("start {outstanding:?}"),
            QualificationState::Progress {
                witnesses,
                outstanding,
            } => format!("progress {} {outstanding:?}", self.witnesses(witnesses)),
            QualificationState::Complete { witnesses } => {
                format!("complete {}", self.witnesses(witnesses))
            }
            QualificationState::Failed {
                failure,
                witnesses,
                outstanding,
            } => format!(
                "failed {} {} {outstanding:?}",
                self.seq(*failure),
                self.witnesses(witnesses)
            ),
        }
    }

    fn hold_reason(&mut self, reason: &HoldReason) -> String {
        match reason {
            HoldReason::Incomplete {
                credited_s,
                required_s,
            } => format!("incomplete {credited_s} {required_s}"),
            HoldReason::AtCap {} => "at_cap".to_owned(),
            HoldReason::NotQualified {} => "not_qualified".to_owned(),
            HoldReason::Vetoed { veto } => format!("vetoed {}", self.event_id(veto)),
            HoldReason::QuestionPending { boot } => format!("question_pending {boot:?}"),
            HoldReason::EventUnresolved { event } => {
                format!("event_unresolved {}", self.event_id(event))
            }
            HoldReason::Frozen {} => "frozen".to_owned(),
            HoldReason::Paused {} => "paused".to_owned(),
            HoldReason::Expired {} => "expired".to_owned(),
            HoldReason::QualificationFailed { profile, witnesses } => {
                format!("qualification_failed {profile:?} {}", self.seqs(witnesses))
            }
        }
    }

    fn retreat_cause(&mut self, cause: &RetreatCause) -> String {
        match cause {
            RetreatCause::ShallowerCap {} => "shallower_cap".to_owned(),
            RetreatCause::Event { event, escalation } => {
                format!("event {} {escalation:?}", self.event_id(event))
            }
            RetreatCause::Completion {} => "completion".to_owned(),
        }
    }

    fn step_kind(&mut self, kind: &StepKind) -> String {
        match kind {
            StepKind::Hold { blocked_by } => {
                let reasons: Vec<_> = blocked_by
                    .iter()
                    .map(|reason| self.hold_reason(reason))
                    .collect();
                format!("hold [{}]", reasons.join(", "))
            }
            StepKind::Promote { changed } => format!("promote {changed:?}"),
            StepKind::Retreat {
                changed,
                cause,
                wanted,
            } => format!(
                "retreat {changed:?} {} {wanted:?}",
                self.retreat_cause(cause)
            ),
            StepKind::FirmwareZero { changed } => format!("firmware_zero {changed:?}"),
        }
    }

    fn step_causes(&mut self, causes: &StepCauses) -> String {
        let StepCauses {
            caps,
            qualification,
            vetoes,
            events,
        } = causes;
        format!(
            "caps={} qualification={} vetoes={} events={}",
            self.seqs(caps),
            self.opt_seq(*qualification),
            self.seqs(vetoes),
            self.seqs(events)
        )
    }

    fn day_step(&mut self, step: &DayStep) -> String {
        let DayStep {
            decision,
            cold_boot,
            old,
            proposed,
            step: kind,
            predecessor,
            hours,
            hold_s,
            causes,
            reason,
        } = step;
        format!(
            "decision={decision:?} cold_boot={cold_boot:?} old={old:?} proposed={proposed:?} step={} \
             predecessor={predecessor:?} hours={} hold_s={hold_s} causes={} reason={reason:?}",
            self.step_kind(kind),
            self.seqs(hours),
            self.step_causes(causes)
        )
    }

    fn apply_cause(&mut self, cause: &ApplyCause) -> String {
        match cause {
            ApplyCause::Step { decision } => format!("step {decision:?}"),
            ApplyCause::Reapplication {} => "reapplication".to_owned(),
            ApplyCause::Reset { cause } => format!("reset {}", self.seq(*cause)),
        }
    }

    fn hold_change(&mut self, change: &HoldChange) -> String {
        match change {
            HoldChange::Credited {} => "credited".to_owned(),
            HoldChange::Reset { cause } => format!("reset {}", self.seq(*cause)),
            HoldChange::Recomputed { cause } => format!("recomputed {}", self.seq(*cause)),
        }
    }

    fn unavailable(&mut self, unavailable: &[UnavailableBackend]) -> String {
        let backends: Vec<_> = unavailable
            .iter()
            .map(|entry| {
                let UnavailableBackend { backend, causes } = entry;
                format!("{backend:?} {}", self.seqs(causes))
            })
            .collect();
        format!("[{}]", backends.join(", "))
    }

    fn exposure_observed(&mut self, observed: &ExposureObserved, hide_source_boot: bool) -> String {
        let ExposureObserved {
            source,
            origin,
            firmware,
            epoch,
            class,
            profile,
            tested,
            activity,
            outcome,
            failures,
            eligible_s,
            uninterrupted_s,
            exclusions,
        } = observed;
        let OriginalSource {
            session,
            seq,
            boot,
            trial,
        } = source;
        let trial = match trial {
            Some(trial) => format!("Some({})", self.trial(trial)),
            None => "None".to_owned(),
        };
        let boot = if hide_source_boot {
            "boot".to_owned()
        } else {
            format!("{boot:?}")
        };
        format!(
            "source={session:?} {} {boot} {trial} origin={origin:?} firmware={firmware:?} epoch={epoch} \
             class={class:?} profile={profile:?} tested={tested:?} activity={activity:?} outcome={outcome:?} \
             failures={failures:?} eligible_s={eligible_s} uninterrupted_s={uninterrupted_s} \
             exclusions={exclusions:?}",
            self.seq(*seq)
        )
    }

    fn ccd_edge(&mut self, edge: &CcdEdge) -> String {
        let CcdEdge {
            ccd,
            edge,
            old,
            reason,
        } = edge;
        format!(
            "{ccd:?} edge={} old={} reason={reason:?}",
            self.edge(edge),
            self.edge(old)
        )
    }

    fn ccd_cap(&mut self, cap: &CcdCap) -> String {
        let CcdCap {
            ccd,
            old,
            new,
            budget,
            edge,
            adequate,
            uncovered,
            cells,
            slope,
            candidates,
            binding,
        } = cap;
        format!(
            "{ccd:?} old={old:?} new={new:?} budget={budget:?} edge={} adequate={adequate:?} \
             uncovered={uncovered:?} cells={cells:?} slope={slope:?} candidates={candidates:?} \
             binding={binding:?}",
            self.edge(edge)
        )
    }

    fn night_plan(&mut self, plan: &NightPlan) -> String {
        let NightPlan {
            night,
            budgets,
            tasks,
            unavailable,
        } = plan;
        format!(
            "night={} budgets={budgets:?} tasks={tasks:?} unavailable={}",
            self.opt_night(*night),
            self.unavailable(unavailable)
        )
    }

    fn night_qualification(&mut self, qualification: &NightQualification) -> String {
        let NightQualification {
            profile,
            firmware,
            epoch,
            contents,
            matrix,
            purpose,
            state,
        } = qualification;
        format!(
            "profile={profile:?} firmware={firmware:?} epoch={epoch} contents={contents:?} matrix={matrix:?} \
             purpose={purpose:?} state={}",
            self.qualification_state(state)
        )
    }

    fn night_reproduction(&mut self, reproduction: &NightReproduction) -> String {
        let NightReproduction {
            source,
            target,
            night,
            consumed_s,
            remaining,
            failures,
            state,
        } = reproduction;
        format!(
            "source={} target={target:?} night={} consumed_s={consumed_s} remaining={remaining:?} \
             failures={} state={state:?}",
            self.event_id(source),
            self.night(*night),
            self.seqs(failures)
        )
    }

    fn day_applied(&mut self, applied: &DayApplied) -> String {
        let DayApplied {
            application,
            cause,
            target,
            readbacks,
            transactions,
            boot,
            mono_ns,
        } = applied;
        format!(
            "application={application:?} cause={} target={target:?} readbacks={readbacks:?} \
             transactions={} boot={boot:?} mono_ns={mono_ns}",
            self.apply_cause(cause),
            self.seqs(transactions)
        )
    }

    fn day_event(&mut self, event: &DayEvent) -> String {
        let DayEvent {
            event: id,
            profile,
            boot,
            from,
            to,
            witnesses,
            classification,
            resolution,
            reason,
            predecessor,
            escalation,
            cursors,
            consumed,
            reproduction,
        } = event;
        format!(
            "event={} profile={profile:?} boot={boot:?} from={from:?} to={to:?} witnesses={} \
             classification={classification:?} resolution={} reason={reason:?} predecessor={} \
             escalation={escalation:?} cursors={cursors:?} consumed={consumed:?} reproduction={reproduction:?}",
            self.event_id(id),
            self.seqs(witnesses),
            self.resolution(resolution),
            self.opt_event_id(predecessor.as_ref())
        )
    }

    fn day_veto(&mut self, veto: &DayVeto) -> String {
        let DayVeto {
            event,
            profile,
            state,
            evidence,
            reason,
        } = veto;
        format!(
            "event={} profile={profile:?} state={state:?} evidence={} reason={reason:?}",
            self.event_id(event),
            self.seqs(evidence)
        )
    }

    fn day_hold(&mut self, hold: &DayHold) -> String {
        let DayHold {
            profile,
            application,
            credited,
            eligible_s,
            change,
        } = hold;
        format!(
            "profile={profile:?} application={application:?} credited={} eligible_s={eligible_s} change={}",
            self.seqs(credited),
            self.hold_change(change)
        )
    }

    fn day_freeze(&mut self, freeze: &DayFreeze) -> String {
        let DayFreeze { choice, source } = freeze;
        let source = match source {
            FreezeSource::Command { uid } => format!("command {uid}"),
            FreezeSource::Imported { from } => format!("imported {}", self.provenance(from)),
        };
        format!("{choice:?} {source}")
    }

    fn day_interval(&mut self, interval: &DayInterval) -> String {
        let DayInterval {
            boot,
            start_ns,
            end_ns,
            profile,
            application,
            state,
            checkpoints,
            reason,
        } = interval;
        format!(
            "boot={boot:?} start_ns={start_ns} end_ns={end_ns} profile={profile:?} application={application:?} \
             state={state:?} checkpoints={} reason={reason:?}",
            self.seqs(checkpoints)
        )
    }

    fn climb_action(&mut self, action: &togi_journal::ClimbAction) -> String {
        use togi_journal::{ClimbAction, ClimbRetreatCause};
        match action {
            ClimbAction::Hold { blocked_by } => {
                let blockers: Vec<_> = blocked_by
                    .iter()
                    .map(|blocker| {
                        use togi_journal::ClimbBlocker;
                        match blocker {
                            ClimbBlocker::CertificateFailed { profile, witnesses } => {
                                format!("certificate_failed {profile:?} {}", self.seqs(witnesses))
                            }
                            ClimbBlocker::EventUnresolved { event } => {
                                format!("event_unresolved {}", self.event_id(event))
                            }
                            ClimbBlocker::Veto { event } => {
                                format!("veto {}", self.event_id(event))
                            }
                            ClimbBlocker::Hold { .. }
                            | ClimbBlocker::Certificate {}
                            | ClimbBlocker::QuestionPending { .. }
                            | ClimbBlocker::FirmwareInvalid {}
                            | ClimbBlocker::Frozen {}
                            | ClimbBlocker::Paused {}
                            | ClimbBlocker::AllowanceExhausted {}
                            | ClimbBlocker::UnknownEdge { .. }
                            | ClimbBlocker::CampaignHistory {}
                            | ClimbBlocker::SameBoot {}
                            | ClimbBlocker::HoldProfile {}
                            | ClimbBlocker::SafeApply {}
                            | ClimbBlocker::ActivationClosed {}
                            | ClimbBlocker::Finished { .. }
                            | ClimbBlocker::Ceiling { .. } => format!("{blocker:?}"),
                        }
                    })
                    .collect();
                format!("hold {blockers:?}")
            }
            ClimbAction::Promote {
                unit,
                from,
                to,
                step,
            } => format!("promote {unit:?} {from:?} {to:?} {step}"),
            ClimbAction::Retreat {
                units,
                cause,
                wanted,
            } => {
                let cause = match cause {
                    ClimbRetreatCause::AdverseCeiling { edges } => {
                        format!("adverse_ceiling {}", self.seqs(edges))
                    }
                    ClimbRetreatCause::Event { event } => format!("event {}", self.event_id(event)),
                    ClimbRetreatCause::Veto { event } => format!("veto {}", self.event_id(event)),
                    ClimbRetreatCause::Completion {} => "completion".to_owned(),
                };
                format!("retreat {units:?} {cause} {wanted:?}")
            }
            ClimbAction::Restore { restoration, units } => {
                format!("restore {restoration:?} {units:?}")
            }
            ClimbAction::FirmwareZero {} => "firmware_zero".to_owned(),
        }
    }

    fn unit_stop_reason(&mut self, reason: &togi_journal::UnitStopReason) -> String {
        use togi_journal::UnitStopReason;
        match reason {
            UnitStopReason::Ceiling {} => "ceiling".to_owned(),
            UnitStopReason::AllowanceExhausted {} => "allowance_exhausted".to_owned(),
            UnitStopReason::AdverseCeiling {} => "adverse_ceiling".to_owned(),
            UnitStopReason::Veto {} => "veto".to_owned(),
            UnitStopReason::Predecessor { original } => {
                format!("predecessor {}", self.provenance(original))
            }
            UnitStopReason::FirstBackstep { event, application } => {
                format!("first_backstep {} {application:?}", self.event_id(event))
            }
            UnitStopReason::RestorationBackstep { event } => {
                format!("restoration_backstep {}", self.event_id(event))
            }
        }
    }

    /// A policy commitment or an exposure observation, named: its shape and what it cites. Every other payload is
    /// no commitment; the caller selects which are compared.
    pub(super) fn payload(&mut self, payload: &Payload) -> String {
        self.rendered_payload(payload, false)
            .unwrap_or_else(|| panic!("{} is compared by no rule", payload.kind().as_str()))
    }

    /// One nonrecursive anchor layer: hidden names never look up another target.
    fn rendered_payload(&mut self, payload: &Payload, hide_source_boot: bool) -> Option<String> {
        Some(match payload {
            Payload::DeadEnd(dead_end) => format!("{dead_end:?}"),
            Payload::ExposureObserved(observed) => {
                self.exposure_observed(observed, hide_source_boot)
            }
            Payload::CcdEdge(edge) => self.ccd_edge(edge),
            Payload::CcdCap(cap) => self.ccd_cap(cap),
            Payload::NightPlan(plan) => self.night_plan(plan),
            Payload::NightQualification(qualification) => self.night_qualification(qualification),
            Payload::NightReproduction(reproduction) => self.night_reproduction(reproduction),
            Payload::DayStep(step) => self.day_step(step),
            Payload::DayApplied(applied) => self.day_applied(applied),
            Payload::DayEvent(event) => self.day_event(event),
            Payload::DayVeto(veto) => self.day_veto(veto),
            Payload::DayHold(hold) => self.day_hold(hold),
            Payload::DayFreeze(freeze) => self.day_freeze(freeze),
            Payload::DayInterval(interval) => self.day_interval(interval),
            Payload::UnitMapping(mapping) => format!("{mapping:?}"),
            Payload::UnitEdge(edge) => {
                let togi_journal::UnitEdge {
                    unit,
                    firmware,
                    epoch,
                    old,
                    new,
                    ceiling,
                    margin,
                } = edge;
                format!(
                    "{unit:?} {firmware:?} {epoch} old={} new={} {ceiling:?} {margin}",
                    self.edge(old),
                    self.edge(new)
                )
            }
            Payload::ClimbDecision(decision) => {
                let togi_journal::ClimbDecision {
                    decision,
                    cold_boot,
                    before,
                    proposed,
                    hold,
                    certificate,
                    ceilings,
                    events,
                } = decision;
                let togi_journal::ClimbHold {
                    application,
                    credited,
                    eligible_s,
                } = hold;
                format!(
                    "{} {cold_boot:?} {before:?} {proposed:?} hold={application:?}:{}:{eligible_s} cert={} ceilings={} events={}",
                    self.climb_action(decision),
                    self.seqs(credited),
                    self.opt_seq(*certificate),
                    self.seqs(ceilings),
                    self.seqs(events)
                )
            }
            Payload::UnitStop(stop) => {
                let togi_journal::UnitStop {
                    unit,
                    reason,
                    profile,
                    last_clean,
                    witnesses,
                } = stop;
                format!(
                    "{unit:?} {} {profile:?} {last_clean:?} {}",
                    self.unit_stop_reason(reason),
                    self.seqs(witnesses)
                )
            }
            Payload::CampaignStart(start) => {
                let togi_journal::CampaignStart {
                    campaign,
                    allowance,
                    origin,
                    history,
                } = start;
                format!(
                    "{} {allowance} {origin:?} {history:?}",
                    self.provenance(&campaign.0)
                )
            }
            Payload::CampaignCharge(charge) => {
                let togi_journal::CampaignCharge {
                    campaign,
                    event,
                    aliases,
                    profile,
                    phase,
                    unit_phase,
                    provisional,
                    consumed,
                    remaining,
                    exhausted,
                } = charge;
                let aliases: Vec<_> = aliases.iter().map(|event| self.event_id(event)).collect();
                format!(
                    "{} {} {aliases:?} {profile:?} {phase:?} {unit_phase:?} {provisional} {consumed} {remaining} {exhausted}",
                    self.provenance(&campaign.0),
                    self.event_id(event)
                )
            }
            Payload::CampaignReclassified(revised) => {
                let togi_journal::CampaignReclassified {
                    campaign,
                    event,
                    from,
                    to,
                } = revised;
                format!(
                    "{} {} {from:?} {to:?}",
                    self.provenance(&campaign.0),
                    self.event_id(event)
                )
            }
            Payload::FirmwareRestoration(restoration) => {
                use togi_journal::FirmwareRestorationState;
                let togi_journal::FirmwareRestoration { saved, state } = restoration;
                let state = match state {
                    FirmwareRestorationState::Saved {} => "saved".to_owned(),
                    FirmwareRestorationState::Qualified { certificate } => {
                        format!("qualified {}", self.seq(*certificate))
                    }
                    FirmwareRestorationState::Held { hold } => format!(
                        "held {:?} {} {}",
                        hold.application,
                        self.seqs(&hold.credited),
                        hold.eligible_s
                    ),
                    FirmwareRestorationState::Restored { application } => {
                        format!("restored {application:?}")
                    }
                    FirmwareRestorationState::SteppedBack { event } => {
                        format!("stepped_back {}", self.event_id(event))
                    }
                };
                format!("{saved:?} {state}")
            }
            Payload::SuccessorImported(imported) => {
                let togi_journal::SuccessorImported {
                    sources,
                    counts,
                    vetoes,
                    night_observations,
                    campaign,
                } = imported;
                format!("{sources:?} {counts:?} {vetoes:?} {night_observations:?} {campaign:?}")
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionImported(_)
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
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::TaskStart(_)
            | Payload::DayImported(_)
            | Payload::StockAttested(_)
            | Payload::DayPause(_) => return None,
        })
    }

    fn unit_projection(&mut self, unit: &UnitProjection) -> String {
        let UnitProjection {
            unit,
            cores,
            edge,
            cap,
            evidence,
            day,
            remaining,
        } = unit;
        let edge = match edge {
            Some(edge) => format!("Some({})", self.edge(edge)),
            None => "None".to_owned(),
        };
        let cap = match cap {
            Some(cap) => {
                let CapProjection {
                    offset,
                    binding,
                    slope,
                    adequate,
                    uncovered,
                    witness,
                } = cap;
                format!(
                    "Some({offset:?} {binding:?} {slope:?} {adequate:?} {uncovered:?} {})",
                    self.seq(*witness)
                )
            }
            None => "None".to_owned(),
        };
        format!(
            "unit {unit:?} cores={cores:?} edge={edge} cap={cap} evidence={evidence:?} day={day:?} remaining={remaining:?}"
        )
    }

    fn day_projection(&mut self, day: &DayProjection) -> String {
        let DayProjection {
            hold,
            last_step,
            freeze,
            firmware,
            pending_questions,
        } = day;
        let hold = match hold {
            Some(hold) => {
                let HoldProjection {
                    profile,
                    eligible_s,
                    required_s,
                    witness,
                } = hold;
                format!(
                    "Some({profile:?} {eligible_s} {required_s} {})",
                    self.seq(*witness)
                )
            }
            None => "None".to_owned(),
        };
        let last_step = match last_step {
            Some((seq, step)) => format!("Some({} {})", self.seq(*seq), self.day_step(step)),
            None => "None".to_owned(),
        };
        let firmware = match firmware {
            FirmwareValidity::Valid => "valid".to_owned(),
            FirmwareValidity::Invalidated { since } => {
                format!("invalidated {}", self.seq(*since))
            }
            FirmwareValidity::Unknown => "unknown".to_owned(),
        };
        format!(
            "day hold={hold} last_step={last_step} freeze={freeze} firmware={firmware} \
             pending_questions={pending_questions:?}"
        )
    }

    fn night_projection(&mut self, night: &NightProjection) -> String {
        let NightProjection {
            night,
            started,
            closed_by,
            budgets,
            tasks,
            current,
            mapping,
            qualification,
            reproductions,
            unavailable,
            filler,
        } = night;
        let reproductions: Vec<_> = reproductions
            .iter()
            .map(|reproduction| {
                let ReproductionProjection {
                    source,
                    state,
                    consumed_s,
                    remaining,
                } = reproduction;
                format!(
                    "{} {state:?} {consumed_s} {remaining:?}",
                    self.event_id(source)
                )
            })
            .collect();
        format!(
            "night {} started={started:?} closed_by={} budgets={budgets:?} tasks={tasks:?} current={current:?} \
             mapping={mapping:?} qualification={qualification:?} reproductions=[{}] unavailable={} filler={filler}",
            self.night(*night),
            self.opt_seq(*closed_by),
            reproductions.join(", "),
            self.unavailable(unavailable)
        )
    }

    /// The tuner's projection as lines, one per field or entry.
    pub(super) fn projection(&mut self, projection: &Projection) -> Vec<String> {
        let Projection {
            applied,
            units,
            day,
            events,
            vetoes,
            qualifications,
            night,
            stopped,
            climb,
        } = projection;
        let mut lines = vec![format!("applied {applied:?}")];
        lines.extend(units.iter().map(|unit| self.unit_projection(unit)));
        lines.push(self.day_projection(day));
        for entry in events {
            let EventProjection {
                event,
                classification,
                resolution,
                reproduction,
            } = entry;
            lines.push(format!(
                "event {} {classification:?} {} {reproduction:?}",
                self.event_id(event),
                self.resolution(resolution)
            ));
        }
        for entry in vetoes {
            let VetoProjection {
                event,
                state,
                source,
            } = entry;
            lines.push(format!(
                "veto {} {state:?} {source:?}",
                self.event_id(event)
            ));
        }
        for entry in qualifications {
            let QualificationProjection {
                profile,
                purpose,
                state,
                current,
                eligible_s,
            } = entry;
            lines.push(format!(
                "qualification {profile:?} {purpose:?} {state:?} current={current} eligible_s={eligible_s}"
            ));
        }
        lines.push(match night {
            Some(night) => self.night_projection(night),
            None => "night None".to_owned(),
        });
        lines.push(format!("stopped {stopped}"));
        let mut value = serde_json::to_value(climb).unwrap();
        self.climb_citations(&mut value);
        lines.push(format!("climb {value}"));
        lines
    }

    /// Preserve every projection field while naming local evidence by the same bounded anchors.
    fn climb_citations(&mut self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "witness" || key == "certificate" {
                        if let Some(seq) = value.as_u64().and_then(Seq::new) {
                            *value = serde_json::Value::String(self.seq(seq));
                        }
                    } else if key == "witnesses" {
                        if let Some(values) = value.as_array_mut() {
                            for value in values {
                                if let Some(seq) = value.as_u64().and_then(Seq::new) {
                                    *value = serde_json::Value::String(self.seq(seq));
                                } else {
                                    self.climb_citations(value);
                                }
                            }
                        }
                    } else {
                        self.climb_citations(value);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    self.climb_citations(value);
                }
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
}
