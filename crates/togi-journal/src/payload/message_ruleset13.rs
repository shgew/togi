//! `msg` texts of the Ruleset 13 kinds: what a stranger reading the journal needs, without consulting `data`.

use std::fmt::Write as _;

use togi_machine::TargetOffset;

use super::super::domain::{
    Certainty, FailureTime, FreezeChoice, IntervalState, Outcome, QualificationPurpose,
    ReproductionState, VetoState,
};
use super::super::ruleset13::{
    Binding, CcdCap, CcdEdge, CcdVector, DayEventId, Edge, ExposureClass, ExposureObserved,
    LowerEndpoint, NightPlan, NightQualification, NightReproduction, ObservedProfile, Position,
    QualificationState, ReproductionTarget, Slope, Tested,
};
use super::super::ruleset13_day::{
    ApplyCause, Classification, DayApplied, DayEvent, DayFreeze, DayHold, DayImported, DayInterval,
    DayStep, DayVeto, Escalation, FreezeSource, HoldChange, HoldReason, Resolution, RetreatCause,
    SessionImported, StepKind,
};
use super::{core, cores, with_reason};

fn offset(offset: TargetOffset) -> String {
    offset.get().to_string()
}

fn ccd_vector(profile: &CcdVector) -> String {
    let parts: Vec<_> = profile
        .ccd
        .iter()
        .map(|(ccd, at)| format!("CCD{ccd} {}", offset(at)))
        .collect();
    parts.join(", ")
}

fn observed(profile: &ObservedProfile) -> String {
    match profile {
        ObservedProfile::Ccd { ccd, .. } => {
            let parts: Vec<_> = ccd
                .iter()
                .map(|(ccd, at)| format!("CCD{ccd} {}", offset(at)))
                .collect();
            parts.join(", ")
        }
        ObservedProfile::PerCore { cores } => {
            let parts: Vec<_> = cores
                .iter()
                .map(|(id, at)| format!("{}: {}", core(id), at.0))
                .collect();
            format!("per-core [{}]", parts.join(", "))
        }
    }
}

fn event_id(id: &DayEventId) -> String {
    match id {
        DayEventId::Boot { boot } => format!("boot {}", super::short_boot(boot.as_str())),
        DayEventId::Record { session, seq } => format!("record {session}#{seq}"),
        DayEventId::Requalification { night } => format!("re-qualification of night {night}"),
    }
}

fn class(class: &ExposureClass) -> String {
    let place = match class.position {
        Some(Position::Light { core: id }) => format!("light core {}", core(id)),
        Some(Position::Serial { cpu }) => format!("serial cpu {cpu}"),
        None if class.cores.len() == 1 => format!("core {}", cores(class.cores)),
        None => format!("cores {}", cores(class.cores)),
    };
    format!("{} {place} {} s", class.regime, class.duration_s)
}

fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

pub(super) fn exposure_observed(p: &ExposureObserved) -> String {
    let failures = match (p.failures.len(), p.outcome) {
        (0, Outcome::Inconclusive) => "inconclusive, no failure".to_owned(),
        (0, Outcome::Pass | Outcome::Failure) => "no failure".to_owned(),
        (n, _) => {
            let early = p
                .failures
                .iter()
                .filter(|failure| failure.time == FailureTime::WarmUp)
                .count();
            let mut text = count(n, "failure");
            if early > 0 {
                let _ = write!(text, " ({early} before warm-up ended)");
            }
            text
        }
    };
    let place = match p.class.position {
        Some(Position::Light { core: id }) => format!("light core {}", core(id)),
        Some(Position::Serial { cpu }) => format!("serial cpu {cpu}"),
        None if p.class.cores.len() == 1 => format!("core {}", cores(p.class.cores)),
        None => format!("cores {}", cores(p.class.cores)),
    };
    let at = match p.tested {
        Tested::Isolated { ccd, offset: at } => {
            format!("CCD{ccd} {} {place} at {}", p.class.regime, offset(at))
        }
        Tested::Exact {} => format!("exact profile {} {place}", p.class.regime),
    };
    let mut text = format!("{at}: {} s eligible, {failures}", p.eligible_s);
    if !p.exclusions.is_empty() {
        text.push_str("; credit withheld");
    }
    text
}

fn edge(edge: &Edge) -> String {
    match edge {
        Edge::Unknown {} => "unknown".to_owned(),
        Edge::Observed { offset: at, .. } => format!("failing at {}", offset(*at)),
        Edge::Censored { limit, .. } => format!("censored at the write limit {}", offset(*limit)),
    }
}

pub(super) fn ccd_edge(p: &CcdEdge) -> String {
    with_reason(
        format!("CCD{} edge U {} -> {}", p.ccd, edge(&p.old), edge(&p.edge)),
        Some(&p.reason),
    )
}

pub(super) fn ccd_cap(p: &CcdCap) -> String {
    let slope = match p.slope {
        Slope::Fallback { beta } => format!("fallback slope {:.3}", beta.get()),
        Slope::Measured {
            lower: LowerEndpoint::Bounded { beta },
            ..
        } => format!("measured slope lower endpoint {:.3}", beta.get()),
        Slope::Measured {
            lower: LowerEndpoint::Unbounded {},
            ..
        } => "measured slope with no lower endpoint".to_owned(),
    };
    let binding = match &p.binding {
        Binding::Rate { class: bound } => format!("rate of {}", class(bound)),
        Binding::EdgeMargin {} => "the edge margin".to_owned(),
        Binding::Baseline {} => "no justified depth, CO 0".to_owned(),
    };
    format!(
        "CCD{} cap S {} -> {}: {} adequate, {} uncovered; {slope}; bound by {binding}; edge {}",
        p.ccd,
        offset(p.old),
        offset(p.new),
        count(p.adequate.len(), "class"),
        p.uncovered.len(),
        edge(&p.edge)
    )
}

pub(super) fn night_plan(p: &NightPlan) -> String {
    let budgets: Vec<_> = p
        .budgets
        .iter()
        .map(|budget| {
            format!(
                "{} {} s ({}%)",
                budget.queue, budget.consumed_s, budget.share_pct
            )
        })
        .collect();
    let unavailable: Vec<String> = p
        .unavailable
        .iter()
        .map(|excluded| excluded.backend.to_string())
        .collect();
    format!(
        "{}: consumed {}; {}{}",
        p.night.map_or_else(
            || "night opens".to_owned(),
            |night| format!("night {night}")
        ),
        if budgets.is_empty() {
            "none".to_owned()
        } else {
            budgets.join(", ")
        },
        count(p.tasks.len(), "task"),
        if unavailable.is_empty() {
            String::new()
        } else {
            format!("; unavailable: {}", unavailable.join(", "))
        }
    )
}

pub(super) fn night_qualification(p: &NightQualification) -> String {
    let purpose = match p.purpose {
        QualificationPurpose::NextStep => "next step",
        QualificationPurpose::Requalification => "re-qualification",
        QualificationPurpose::Firmware => "firmware",
    };
    let state = match &p.state {
        QualificationState::Start { outstanding } => {
            format!("started, {} outstanding", count(outstanding.len(), "cell"))
        }
        QualificationState::Progress {
            witnesses,
            outstanding,
        } => format!(
            "{} done, {} outstanding",
            count(witnesses.len(), "cell"),
            outstanding.len()
        ),
        QualificationState::Complete { witnesses } => {
            format!("complete with {}", count(witnesses.len(), "witnessed cell"))
        }
        QualificationState::Failed { outstanding, .. } => format!(
            "failed; {} left outstanding",
            count(outstanding.len(), "cell")
        ),
    };
    format!(
        "qualification of {} ({purpose}): {state}",
        ccd_vector(&p.profile)
    )
}

pub(super) fn night_reproduction(p: &NightReproduction) -> String {
    let target = match &p.target {
        None => "no target (nonuniform profile)".to_owned(),
        Some(ReproductionTarget::Exact { profile }) => {
            format!("exact profile {}", ccd_vector(profile))
        }
        Some(ReproductionTarget::Isolated { ccd, offset: at }) => {
            format!("CCD{ccd} alone at {}", offset(*at))
        }
    };
    let state = match p.state {
        ReproductionState::Pending => "pending",
        ReproductionState::Reproduced => "reproduced",
        ReproductionState::Unreproduced => "unreproduced",
        ReproductionState::Untestable => "untestable",
    };
    format!(
        "reproduction of {} on {target}: {state}; {} s used on night {}, {} left",
        event_id(&p.source),
        p.consumed_s,
        p.night,
        count(p.remaining.len(), "cell")
    )
}

fn changed(ccds: &[togi_machine::CcdId]) -> String {
    ccds.iter()
        .map(|ccd| format!("CCD{ccd}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn escalation(escalation: &Escalation) -> String {
    match escalation {
        Escalation::RevertNewest { reverted } => {
            format!("reverting the newest step (decision {})", reverted.0)
        }
        Escalation::EveryCcd { strike } => format!("every CCD one step back (strike {strike})"),
    }
}

pub(super) fn day_step(p: &DayStep) -> String {
    let text = match &p.step {
        StepKind::Hold { blocked_by } => {
            let why: Vec<_> = blocked_by
                .iter()
                .map(|reason| match reason {
                    HoldReason::Incomplete {
                        credited_s,
                        required_s,
                    } => format!("{credited_s} of {required_s} hold seconds"),
                    HoldReason::AtCap {} => "at the cap".to_owned(),
                    HoldReason::NotQualified {} => "profile not qualified".to_owned(),
                    HoldReason::Vetoed { veto } => format!("vetoed by {}", event_id(veto)),
                    HoldReason::QuestionPending { boot } => {
                        format!(
                            "question about boot {} pending",
                            super::short_boot(boot.as_str())
                        )
                    }
                    HoldReason::EventUnresolved { event } => {
                        format!("{} unresolved", event_id(event))
                    }
                    HoldReason::Frozen {} => "frozen".to_owned(),
                    HoldReason::Paused {} => "paused".to_owned(),
                    HoldReason::Expired {} => "qualification expired".to_owned(),
                    HoldReason::QualificationFailed { profile, witnesses } => {
                        let witnesses: Vec<_> = witnesses.iter().map(ToString::to_string).collect();
                        format!(
                            "{} failed qualification (witnesses #{})",
                            ccd_vector(profile),
                            witnesses.join(", #")
                        )
                    }
                })
                .collect();
            format!(
                "hold day profile {}: {}",
                ccd_vector(&p.old),
                if why.is_empty() {
                    "nothing to change".to_owned()
                } else {
                    why.join("; ")
                }
            )
        }
        StepKind::Promote { changed } => format!(
            "promote CCD{changed} from {} to {} (proposal; not applied)",
            ccd_vector(&p.old),
            ccd_vector(&p.proposed)
        ),
        StepKind::Retreat {
            changed: ccds,
            cause,
            wanted,
        } => {
            let because = match cause {
                RetreatCause::ShallowerCap {} => "a cap became shallower".to_owned(),
                RetreatCause::Event {
                    event,
                    escalation: how,
                } => format!("{}, {}", event_id(event), escalation(how)),
                RetreatCause::Completion {} => "a held retreat is now qualified".to_owned(),
            };
            let awaiting = wanted.as_ref().map_or_else(String::new, |wanted| {
                format!("; CO 0 holds until {} is qualified", ccd_vector(wanted))
            });
            format!(
                "retreat {} from {} to {} because {because}{awaiting} (proposal; not applied)",
                changed(ccds),
                ccd_vector(&p.old),
                ccd_vector(&p.proposed)
            )
        }
        StepKind::FirmwareZero { changed: ccds } => format!(
            "firmware changed: set {} from {} to {} (proposal; not applied)",
            changed(ccds),
            ccd_vector(&p.old),
            ccd_vector(&p.proposed)
        ),
    };
    with_reason(
        format!("decision {}: {text}", p.decision.0),
        Some(&p.reason),
    )
}

pub(super) fn day_applied(p: &DayApplied) -> String {
    let cause = match p.cause {
        ApplyCause::Step { decision } => format!("decision {}", decision.0),
        ApplyCause::Reapplication {} => "reapplication of the unchanged day profile".to_owned(),
        ApplyCause::Reset { .. } => "tuner-directed reset".to_owned(),
    };
    format!(
        "applied day profile {} and read it back ({cause}); application {}",
        ccd_vector(&p.target),
        p.application.0
    )
}

pub(super) fn day_event(p: &DayEvent) -> String {
    let classification = match p.classification {
        Classification::CpuSuspect {
            certainty: Certainty::Definite,
        } => "CPU-suspect",
        Classification::CpuSuspect {
            certainty: Certainty::Provisional,
        } => "provisionally CPU-suspect",
        Classification::NonCpu {} => "not CPU",
        Classification::Unexplained {} => "unexplained",
    };
    let resolution = match p.resolution {
        Resolution::Unresolved {} => "unresolved",
        Resolution::Resolved { .. } => "resolved",
    };
    with_reason(
        format!(
            "{} at {}: {classification}, {resolution}",
            event_id(&p.event),
            observed(&p.profile)
        ),
        Some(&p.reason),
    )
}

pub(super) fn day_veto(p: &DayVeto) -> String {
    let state = match p.state {
        VetoState::Active => "active",
        VetoState::Cleared => "cleared",
    };
    with_reason(
        format!(
            "veto on {} from {}: {state}",
            observed(&p.profile),
            event_id(&p.event)
        ),
        Some(&p.reason),
    )
}

pub(super) fn day_hold(p: &DayHold) -> String {
    let change = match p.change {
        HoldChange::Credited {} => "credited",
        HoldChange::Reset { .. } => "reset",
        HoldChange::Recomputed { .. } => "recomputed",
    };
    format!(
        "hold at {} {change}: {} s eligible from {}",
        ccd_vector(&p.profile),
        p.eligible_s,
        count(p.credited.len(), "interval")
    )
}

pub(super) fn day_freeze(p: &DayFreeze) -> String {
    let choice = match p.choice {
        FreezeChoice::Freeze => "frozen",
        FreezeChoice::Unfreeze => "unfrozen",
    };
    let by = match p.source {
        FreezeSource::Command { uid } => format!("uid {uid}"),
        FreezeSource::Imported { from } => format!("imported from {}#{}", from.session, from.seq),
    };
    format!("promotion {choice} by {by}")
}

pub(super) fn day_interval(p: &DayInterval) -> String {
    let state = match p.state {
        IntervalState::Applied => "applied",
        IntervalState::Paused => "paused",
        IntervalState::Suspended => "suspended",
        IntervalState::Unobserved => "unobserved",
        IntervalState::Ambiguous => "ambiguous",
    };
    with_reason(
        format!(
            "{} s {state} at {}",
            p.end_ns.saturating_sub(p.start_ns) / 1_000_000_000,
            observed(&p.profile)
        ),
        Some(&p.reason),
    )
}

pub(super) fn day_imported(p: &DayImported) -> String {
    format!(
        "imported {} {} from session {} seq {}",
        p.class, p.original_kind, p.original.session, p.original.seq
    )
}

pub(super) fn session_imported(p: &SessionImported) -> String {
    with_reason(
        format!(
            "imported {} of {} selected records from {}; {} unknown, {} excluded, {} duplicate",
            p.counts.imported,
            p.counts.selected,
            count(p.sources.len(), "session"),
            p.counts.unknown,
            p.counts.excluded,
            p.counts.duplicates
        ),
        Some(&p.reason),
    )
}
