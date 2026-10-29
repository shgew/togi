//! Bounded-exploration presentation. All policy facts arrive through the shared projection.

use std::fmt::Write as _;

use togi_journal::{
    CampaignEventPhase, CampaignHistory, Classification, ClimbAction, ClimbBlocker, DayEventId,
    Edge, FirmwareRestorationState, UnitStopReason, UnitVector,
};
use togi_tuner::{
    ClimbGoalSupport, ClimbPermissionMode, ClimbProjection, ClimbQualificationState,
    ClimbUnitState, Phase,
};

use super::esc;
use super::policy::{Where, class_word, hours};
use crate::table::{Row, table, wrap};

fn witnesses(seqs: &[togi_journal::Seq]) -> String {
    seqs.iter()
        .map(|seq| format!("#{seq}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn event(id: &DayEventId) -> String {
    match id {
        DayEventId::Boot { boot } => format!("boot {}", esc(boot.as_str())),
        DayEventId::Record { session, seq } => format!("record #{seq} of session {session}"),
        DayEventId::Requalification { night } => format!("requalification of night #{night}"),
    }
}

fn vector(profile: &UnitVector) -> String {
    profile
        .cores
        .iter()
        .map(|(core, offset)| format!("core {core} {}", offset.get()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn line(out: &mut String, label: &str, text: &str) {
    wrap(out, label, "  ", text);
}

fn edge(edge: &Edge) -> String {
    match edge {
        Edge::Unknown {} => "unknown".to_owned(),
        Edge::Observed { offset, witness } => format!("observed {} [#{witness}]", offset.get()),
        Edge::Censored {
            limit,
            witnesses: seqs,
        } => format!("censored {} [{}]", limit.get(), witnesses(seqs)),
    }
}

fn blocker(reason: &ClimbBlocker) -> String {
    match reason {
        ClimbBlocker::Hold {
            credited_s,
            required_s,
        } => format!("hold {}/{} h", hours(*credited_s), hours(*required_s)),
        ClimbBlocker::Certificate {} => "exact-profile qualification required".to_owned(),
        ClimbBlocker::CertificateFailed {
            witnesses: seqs, ..
        } => format!("exact proposal failed qualification [{}]", witnesses(seqs)),
        ClimbBlocker::QuestionPending { boot } => {
            format!("question pending for boot {}", esc(boot.as_str()))
        }
        ClimbBlocker::EventUnresolved { event: id } => format!("{} unresolved", event(id)),
        ClimbBlocker::Veto { event: id } => format!("veto from {}", event(id)),
        ClimbBlocker::FirmwareInvalid {} => "firmware invalid".to_owned(),
        ClimbBlocker::Frozen {} => "frozen".to_owned(),
        ClimbBlocker::Paused {} => "paused".to_owned(),
        ClimbBlocker::AllowanceExhausted {} => "allowance exhausted".to_owned(),
        ClimbBlocker::UnknownEdge { unit } => {
            format!("unknown edge for {}", journal_unit_name(*unit))
        }
        ClimbBlocker::Ceiling { unit } => {
            format!("{} is at its ceiling", journal_unit_name(*unit))
        }
        ClimbBlocker::Finished { unit } => format!("{} already finished", journal_unit_name(*unit)),
        ClimbBlocker::CampaignHistory {} => "campaign history unavailable".to_owned(),
        ClimbBlocker::SameBoot {} => {
            "cold boot required; restart or resume cannot promote".to_owned()
        }
        ClimbBlocker::HoldProfile {} => "exact applied profile hold required".to_owned(),
        ClimbBlocker::SafeApply {} => "safe-apply prerequisites incomplete".to_owned(),
        ClimbBlocker::ActivationClosed {} => "nonzero activation closed; stock only".to_owned(),
    }
}

fn finish(reason: &UnitStopReason) -> String {
    match reason {
        UnitStopReason::Ceiling {} => "ceiling".to_owned(),
        UnitStopReason::AllowanceExhausted {} => "allowance exhausted".to_owned(),
        UnitStopReason::AdverseCeiling {} => "stopped: adverse ceiling".to_owned(),
        UnitStopReason::Veto {} => "stopped: whole-profile veto".to_owned(),
        UnitStopReason::Predecessor { original } => format!(
            "stopped: predecessor event {}:{}",
            original.session,
            original.seq
        ),
        UnitStopReason::FirstBackstep {
            event: id,
            application,
        } => format!(
            "stopped: first backstep after {}, application {application:?}",
            event(id)
        ),
        UnitStopReason::RestorationBackstep { event: id } => {
            format!("stopped: restoration backstep after {}", event(id))
        }
    }
}

fn state(state: &ClimbUnitState) -> String {
    match state {
        ClimbUnitState::Climbing => "climbing".to_owned(),
        ClimbUnitState::Held { reason } => format!(
            "held: {}",
            reason.iter().map(blocker).collect::<Vec<_>>().join("; ")
        ),
        ClimbUnitState::Finished {
            reason, last_clean, ..
        } => format!(
            "finished: {}; last clean {}",
            finish(reason),
            last_clean.get()
        ),
    }
}

fn unit_name(unit: togi_tuner::UnitId) -> String {
    match unit {
        togi_tuner::UnitId::Ccd(id) => format!("CCD {id}"),
        togi_tuner::UnitId::Core(id) => format!("core {id}"),
    }
}

fn journal_unit_name(unit: togi_journal::UnitId) -> String {
    match unit {
        togi_journal::UnitId::Ccd(id) => format!("CCD {id}"),
        togi_journal::UnitId::Core(id) => format!("core {id}"),
    }
}

fn action_word(action: &ClimbAction) -> String {
    match action {
        ClimbAction::Hold { blocked_by } => format!(
            "hold: {}",
            blocked_by
                .iter()
                .map(blocker)
                .collect::<Vec<_>>()
                .join("; ")
        ),
        ClimbAction::Promote {
            unit,
            from,
            to,
            step,
        } => format!(
            "promotion of {} from {} to {} ({step} counts)",
            journal_unit_name(*unit),
            from.get(),
            to.get()
        ),
        ClimbAction::Retreat { cause, wanted, .. } => {
            let reason = match cause {
                togi_journal::ClimbRetreatCause::AdverseCeiling { edges } => {
                    format!("adverse ceiling [{}]", witnesses(edges))
                }
                togi_journal::ClimbRetreatCause::Event { event: id } => {
                    format!("CPU-suspect {}", event(id))
                }
                togi_journal::ClimbRetreatCause::Veto { event: id } => {
                    format!("veto from {}", event(id))
                }
                togi_journal::ClimbRetreatCause::Completion {} => {
                    "previously held safety target now qualified".to_owned()
                }
            };
            let mut text = format!("retreat: {reason}");
            if let Some(profile) = wanted {
                let _ = write!(
                    text,
                    "; wanted {} is not qualified, so falls back to stock",
                    vector(profile)
                );
            }
            text
        }
        ClimbAction::Restore { restoration, .. } => format!("restoration ({restoration:?})"),
        ClimbAction::FirmwareZero {} => "firmware zero".to_owned(),
    }
}

pub(super) fn unit_table(out: &mut String, policy: &ClimbProjection) {
    if policy.units.is_empty() {
        return;
    }
    let mut rows = vec![Row::new(
        ["UNIT", "CORES", "D", "C", "D-C", "HOLD h", "STATE"].map(String::from),
    )];
    for unit in &policy.units {
        rows.push(Row::new([
            unit_name(unit.unit),
            unit.cores
                .iter()
                .map(|core| core.to_string())
                .collect::<Vec<_>>()
                .join(","),
            unit.applied.get().to_string(),
            unit.ceiling
                .map_or("unknown".to_owned(), |c| c.get().to_string()),
            unit.remaining.map_or("-".to_owned(), |r| r.to_string()),
            format!(
                "{}/{}",
                hours(unit.hold.credited_s),
                hours(unit.hold.required_s)
            ),
            match unit.state {
                ClimbUnitState::Climbing => "climbing",
                ClimbUnitState::Held { .. } => "held",
                ClimbUnitState::Finished { .. } => "finished",
            }
            .to_owned(),
        ]));
    }
    table(out, &rows);
}

pub(super) fn sections(out: &mut String, policy: &ClimbProjection, at: Where<'_>) {
    unit_table(out, policy);
    out.push_str("\nDay policy\n");
    match policy.permission {
        ClimbPermissionMode::ExperimentalHoldExactNightQualification => {
            out.push_str("permission: experimental hold + exact night qualification\n")
        }
    }
    out.push_str("S: diagnostic, non-authorizing; finishing is not a safety certificate\n");
    let profiles = &policy.profiles;
    line(
        out,
        "applied profile: ",
        &profiles
            .applied
            .as_ref()
            .map_or("unavailable".to_owned(), vector),
    );
    line(
        out,
        "read-back profile: ",
        &profiles
            .read_back
            .as_ref()
            .map_or("unavailable".to_owned(), |cores| {
                cores
                    .iter()
                    .map(|(core, offset)| format!("core {core} {}", offset.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
    );
    line(
        out,
        "proposed profile: ",
        &profiles.proposed.as_ref().map_or("none".to_owned(), vector),
    );
    line(
        out,
        "paused-saved profile: ",
        &profiles
            .paused_saved
            .as_ref()
            .map_or("none".to_owned(), vector),
    );
    match &policy.campaign {
        None => out.push_str("allowance: unavailable; campaign history must be reconstructed\n"),
        Some(campaign) => {
            line(
                out,
                "allowance: ",
                &format!(
                    "{} consumed / {} limit; {} remaining; {} provisional; {} reclassified{}",
                    campaign.consumed,
                    campaign.limit,
                    campaign.remaining,
                    campaign.provisional,
                    campaign.reclassified,
                    if campaign.exhausted {
                        "; exhausted"
                    } else {
                        ""
                    }
                ),
            );
            match &campaign.history {
                CampaignHistory::Complete {} => {}
                CampaignHistory::Unreconstructible { reason } => {
                    line(out, "campaign history unavailable: ", &esc(reason))
                }
            }
            for observed in &campaign.events {
                let phase = match observed.phase {
                    CampaignEventPhase::Learning => "learning",
                    CampaignEventPhase::PostFinish => "post_finish",
                };
                let classification = match observed.classification {
                    Classification::CpuSuspect { .. } => "CPU-suspect",
                    Classification::NonCpu {} => "non-CPU",
                    Classification::Unexplained {} => "unexplained",
                };
                line(
                    out,
                    "event: ",
                    &format!(
                        "{}: {phase}, {classification}, provisional {}, reclassified {} [{}]",
                        event(&observed.event),
                        observed.provisional,
                        observed.reclassified,
                        witnesses(&observed.witnesses)
                    ),
                );
            }
        }
    }
    for unit in &policy.units {
        let name = unit_name(unit.unit);
        let phase = match unit.phase {
            Phase::Coarse => "coarse",
            Phase::Core => "core",
        };
        let parent = unit.parent.map_or("none".to_owned(), unit_name);
        line(
            out,
            "unit: ",
            &format!(
                "{name}, phase {phase}, parent {parent}, inherited start {}",
                unit.inherited_start.get()
            ),
        );
        line(out, "  U: ", &edge(&unit.edge));
        line(out, "  state: ", &state(&unit.state));
        if let ClimbUnitState::Finished {
            at: time,
            profile,
            witnesses: seqs,
            ..
        } = &unit.state
        {
            line(
                out,
                "  finished at: ",
                &format!("{}; {} [{}]", time, vector(profile), witnesses(seqs)),
            );
        }
        if let Some(proposal) = &unit.next {
            let qualified = match proposal.qualification.state {
                ClimbQualificationState::Missing => "missing".to_owned(),
                ClimbQualificationState::InProgress => "in progress".to_owned(),
                ClimbQualificationState::Complete { certificate } => {
                    format!("complete [#{certificate}]")
                }
                ClimbQualificationState::Failed { .. } => "failed; exact vector blocked".to_owned(),
            };
            line(out, "  next proposal: ", &vector(&proposal.profile));
            line(
                out,
                "  qualification: ",
                &format!(
                    "{qualified}; {} outstanding [{}]",
                    proposal.qualification.outstanding.len(),
                    witnesses(&proposal.qualification.witnesses)
                ),
            );
            for class in &proposal.qualification.outstanding {
                line(out, "    outstanding: ", &class_word(class));
            }
        }
        if let Some(step) = &unit.latest_step {
            let action = action_word(&step.action);
            line(
                out,
                "  latest applied step: ",
                &format!(
                    "{action} at {}; {} -> {} [{}]",
                    at.clock(step.at),
                    vector(&step.before),
                    vector(&step.after),
                    witnesses(&step.witnesses)
                ),
            );
        }
        if let Some(diagnostic) = &unit.diagnostic_s {
            line(
                out,
                "  diagnostic S (non-authorizing): ",
                &format!(
                    "{} [#{}]",
                    diagnostic.cap.offset.get(),
                    diagnostic.cap.witness
                ),
            );
        }
    }
    match &policy.daily {
        None => out.push_str("experimental: current exact profile daily evidence unavailable\n"),
        Some(daily) if profiles.applied.as_ref() == Some(&daily.profile) => {
            line(
                out,
                "current exact profile daily evidence: ",
                &format!(
                    "{} h; {} crashes [{}]",
                    hours(daily.observed_s),
                    daily.crashes,
                    witnesses(&daily.witnesses)
                ),
            );
            match daily.poisson_upper_per_h {
                Some(rate) if rate.is_finite() && rate > 0.0 => {
                    line(
                        out,
                        "experimental: ",
                        &format!(
                            "{} clean hours; at most 1 crash per {:.1} h (95%, fixed-rate model)",
                            hours(daily.clean_s),
                            1.0 / rate
                        ),
                    );
                    if daily.goal == ClimbGoalSupport::MeetsGoal {
                        out.push_str(
                            "meets your goal (current exact profile only; 95%, fixed-rate model)\n",
                        );
                    }
                }
                Some(_) | None => {
                    out.push_str("experimental: rate bound unavailable (95%, fixed-rate model)\n")
                }
            }
        }
        Some(_) => out.push_str(
            "experimental: daily evidence belongs to another profile; no current-profile claim\n",
        ),
    }
    match &policy.firmware_restoration {
        None => out.push_str("firmware restoration: none\n"),
        Some(restoration) => {
            let state = match &restoration.state {
                FirmwareRestorationState::Saved {} => {
                    "saved; awaiting new-context qualification and stock hold".to_owned()
                }
                FirmwareRestorationState::Qualified { certificate } => {
                    format!("qualified in new context [#{certificate}]")
                }
                FirmwareRestorationState::Held { hold } => format!(
                    "held for stock hours: {} h [{}]",
                    hours(hold.eligible_s),
                    witnesses(&hold.credited)
                ),
                FirmwareRestorationState::Restored { application } => format!(
                    "restored in one whole-profile learning step; application {}",
                    application.0
                ),
                FirmwareRestorationState::SteppedBack { event: id } => format!(
                    "stepped back to stock; restored units stopped after {}",
                    event(id)
                ),
            };
            line(out, "firmware restoration: ", &state);
            line(out, "  saved exact profile: ", &vector(&restoration.saved));
        }
    }
}
