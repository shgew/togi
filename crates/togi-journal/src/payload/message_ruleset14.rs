//! Stranger-readable Ruleset 14 commitments; proposals never imply successful application.

use super::super::ruleset14::{
    CampaignCharge, CampaignEventPhase, CampaignHistory, CampaignOrigin, CampaignReclassified,
    CampaignStart, ClimbAction, ClimbBlocker, ClimbDecision, ClimbRetreatCause,
    FirmwareRestoration, FirmwareRestorationState, RestorationKind, SuccessorImported, UnitEdge,
    UnitId, UnitMapping, UnitPhase, UnitStop, UnitStopReason, UnitVector,
};
use super::super::{Classification, DayEventId, Edge};

fn unit(id: UnitId) -> String {
    match id {
        UnitId::Ccd(id) => format!("CCD{id}"),
        UnitId::Core(id) => format!("core {id:02}"),
    }
}

fn units(ids: &[UnitId]) -> String {
    ids.iter()
        .map(|id| unit(*id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn phase(phase: UnitPhase) -> &'static str {
    match phase {
        UnitPhase::Coarse => "coarse",
        UnitPhase::Core => "core",
    }
}

fn profile(vector: &UnitVector) -> String {
    let cores = vector
        .cores
        .iter()
        .map(|(id, at)| format!("{id:02}: {}", at.get()))
        .collect::<Vec<_>>();
    format!("{} [{}]", phase(vector.mapping.phase), cores.join(", "))
}

fn event(id: &DayEventId) -> String {
    match id {
        DayEventId::Boot { boot } => format!("boot {}", super::short_boot(boot.as_str())),
        DayEventId::Record { session, seq } => format!("record {session}#{seq}"),
        DayEventId::Requalification { night } => format!("re-qualification of night {night}"),
    }
}

fn edge(edge: &Edge) -> String {
    match edge {
        Edge::Unknown {} => "unknown".into(),
        Edge::Observed { offset, witness } => {
            format!("observed {} (failure #{witness})", offset.get())
        }
        Edge::Censored { limit, witnesses } => format!(
            "censored at {} ({} mapping witnesses)",
            limit.get(),
            witnesses.len()
        ),
    }
}

pub(super) fn unit_mapping(p: &UnitMapping) -> String {
    let members = p
        .units
        .iter()
        .map(|entry| {
            let parent = entry
                .parent
                .map_or_else(String::new, |id| format!(", inherited from {}", unit(id)));
            format!(
                "{}: cores [{}]{parent}",
                unit(entry.unit),
                super::cores(entry.cores)
            )
        })
        .collect::<Vec<_>>();
    format!("{} unit mapping: {}", phase(p.phase), members.join("; "))
}

pub(super) fn unit_edge(p: &UnitEdge) -> String {
    let ceiling = p.ceiling.map_or_else(
        || "unknown (no ordinary promotion permission)".into(),
        |at| at.get().to_string(),
    );
    format!(
        "{} edge {} -> {}; ceiling {ceiling}, margin {}",
        unit(p.unit),
        edge(&p.old),
        edge(&p.new),
        p.margin
    )
}

fn blocker(blocker: &ClimbBlocker) -> String {
    match blocker {
        ClimbBlocker::Hold {
            credited_s,
            required_s,
        } => format!("hold {credited_s}/{required_s} s"),
        ClimbBlocker::Certificate {} => "exact-profile certificate missing".into(),
        ClimbBlocker::CertificateFailed {
            profile: target,
            witnesses,
        } => format!(
            "{} qualification failed ({} witnesses)",
            profile(target),
            witnesses.len()
        ),
        ClimbBlocker::QuestionPending { boot } => {
            format!("boot {} question pending", super::short_boot(boot.as_str()))
        }
        ClimbBlocker::EventUnresolved { event: id } => format!("{} unresolved", event(id)),
        ClimbBlocker::Veto { event: id } => format!("veto from {}", event(id)),
        ClimbBlocker::FirmwareInvalid {} => "firmware context invalid".into(),
        ClimbBlocker::Frozen {} => "frozen".into(),
        ClimbBlocker::Paused {} => "paused".into(),
        ClimbBlocker::AllowanceExhausted {} => "machine allowance exhausted".into(),
        ClimbBlocker::UnknownEdge { unit: id } => format!("{} edge unknown", unit(*id)),
        ClimbBlocker::Finished { unit: id } => format!("{} finished", unit(*id)),
    }
}

fn restoration(kind: RestorationKind) -> &'static str {
    match kind {
        RestorationKind::Firmware => "firmware",
        RestorationKind::Reclassified => "reclassification",
    }
}

pub(super) fn climb_decision(p: &ClimbDecision) -> String {
    let action = match &p.decision {
        ClimbAction::Hold { blocked_by } => format!(
            "hold: {}",
            blocked_by
                .iter()
                .map(blocker)
                .collect::<Vec<_>>()
                .join("; ")
        ),
        ClimbAction::Promote {
            unit: id,
            from,
            to,
            step,
        } => format!(
            "promote {} from {} to {} by {step}",
            unit(*id),
            from.get(),
            to.get()
        ),
        ClimbAction::Retreat {
            units: ids,
            cause,
            wanted,
        } => {
            let why = match cause {
                ClimbRetreatCause::AdverseCeiling { edges } => {
                    format!("adverse ceiling ({} edge witnesses)", edges.len())
                }
                ClimbRetreatCause::Event { event: id } => event(id),
                ClimbRetreatCause::Veto { event: id } => format!("veto from {}", event(id)),
                ClimbRetreatCause::Completion {} => "held retreat now qualified".into(),
            };
            let pending = wanted.as_ref().map_or_else(String::new, |target| {
                format!("; wanted {} awaits qualification", profile(target))
            });
            format!("retreat {}: {why}{pending}", units(ids))
        }
        ClimbAction::Restore {
            restoration: kind,
            units: ids,
        } => format!("{} restoration of {}", restoration(*kind), units(ids)),
        ClimbAction::FirmwareZero {} => "firmware changed: reset to stock".into(),
    };
    format!(
        "{action}; {} -> {} (proposal; not applied); hold {} s",
        profile(&p.before),
        profile(&p.proposed),
        p.hold.eligible_s
    )
}

pub(super) fn unit_stop(p: &UnitStop) -> String {
    let reason = match &p.reason {
        UnitStopReason::Ceiling {} => "night ceiling reached".into(),
        UnitStopReason::FirstBackstep {
            event: id,
            application,
        } => format!(
            "first backstep after {} (application {})",
            event(id),
            application.0
        ),
        UnitStopReason::AllowanceExhausted {} => "machine allowance exhausted".into(),
        UnitStopReason::RestorationBackstep { event: id } => {
            format!("restoration stepped back after {}", event(id))
        }
    };
    format!(
        "{} finished: {reason}; last clean depth {}; profile {}",
        unit(p.unit),
        p.last_clean.get(),
        profile(&p.profile)
    )
}

fn history(history: &CampaignHistory) -> String {
    match history {
        CampaignHistory::Complete {} => "complete".into(),
        CampaignHistory::Unreconstructible { reason } => {
            format!("unreconstructible: {reason}; new exploration blocked")
        }
    }
}

pub(super) fn campaign_start(p: &CampaignStart) -> String {
    let origin = match &p.origin {
        CampaignOrigin::Fresh {} => "fresh".into(),
        CampaignOrigin::Successor { sources } => {
            format!("successor from {} source sessions", sources.len())
        }
    };
    format!(
        "campaign {}#{}: {origin}; machine allowance {}; history {}",
        p.campaign.0.session,
        p.campaign.0.seq,
        p.allowance,
        history(&p.history)
    )
}

pub(super) fn campaign_charge(p: &CampaignCharge) -> String {
    let when = match p.phase {
        CampaignEventPhase::Learning => "learning",
        CampaignEventPhase::PostFinish => "post-finish",
    };
    format!(
        "campaign {}#{}: {} {} {} event at {}; allowance consumed {}, remaining {}{} ({} aliases)",
        p.campaign.0.session,
        p.campaign.0.seq,
        if p.provisional {
            "provisional"
        } else {
            "definite"
        },
        when,
        event(&p.event),
        profile(&p.profile),
        p.consumed,
        p.remaining,
        if p.exhausted { ", exhausted" } else { "" },
        p.aliases.len()
    )
}

fn classification(value: Classification) -> &'static str {
    match value {
        Classification::CpuSuspect { .. } => "CPU-suspect",
        Classification::NonCpu {} => "non-CPU",
        Classification::Unexplained {} => "unexplained",
    }
}

pub(super) fn campaign_reclassified(p: &CampaignReclassified) -> String {
    format!(
        "campaign {}#{}: {} reclassified {} -> {}; historical allowance not refunded",
        p.campaign.0.session,
        p.campaign.0.seq,
        event(&p.event),
        classification(p.from),
        classification(p.to)
    )
}

pub(super) fn firmware_restoration(p: &FirmwareRestoration) -> String {
    let state = match &p.state {
        FirmwareRestorationState::Saved {} => "saved before firmware reset".into(),
        FirmwareRestorationState::Qualified { certificate } => {
            format!("qualified by new-context certificate #{certificate}")
        }
        FirmwareRestorationState::Held { hold } => format!("stock hold {} s", hold.eligible_s),
        FirmwareRestorationState::Restored { application } => {
            format!("restored by application {}", application.0)
        }
        FirmwareRestorationState::SteppedBack { event: id } => {
            format!("stepped back to stock after {}", event(id))
        }
    };
    format!("firmware restoration {}: {state}", profile(&p.saved))
}

pub(super) fn successor_imported(p: &SuccessorImported) -> String {
    format!(
        "successor imported {} of {} selected raw records from {} sessions; {} vetoes, {} night observations; campaign history {}",
        p.counts.imported,
        p.counts.selected,
        p.sources.len(),
        p.vetoes.len(),
        p.night_observations.len(),
        history(&p.campaign)
    )
}
