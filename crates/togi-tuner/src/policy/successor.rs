//! Reconstruct irreversible campaign facts without carrying predecessor permission.
//!
//! Inputs retain their original sequence namespace. Applications are history only: callers must
//! start the successor at verified stock, and recompute edges and qualification from observations.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;
use togi_journal::{
    ApplyCause, CampaignHistory, Classification, DayApplied, DayEventId, DayImported, Event,
    EventKind, ImportedRecordRef, ObservedProfile, Payload, Provenance, QualificationPurpose,
    QualificationState, Seq, SessionId, StepKind, UnitId, VetoState,
};
use togi_machine::{BootId, TargetOffset};

use super::{Params, State};
use crate::day::EventId;

/// A machine event, independent of the session that copied its witnesses.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Identity {
    Boot(BootId),
    Unobserved(Timestamp),
    Record(Provenance),
    Requalification {
        session: SessionId,
        night: togi_journal::NightId,
    },
}

/// Historical consumption is sticky even when its current classification changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Charge {
    pub event: Identity,
    pub aliases: Vec<Identity>,
    pub profile: ObservedProfile,
    pub provisional: bool,
    pub reclassified: bool,
    pub witnesses: Vec<Provenance>,
    pub phase: togi_journal::CampaignEventPhase,
    pub unit_phase: togi_journal::UnitPhase,
    /// Explicit receipts retain campaign phase and the exact original unit mapping.
    pub receipt: Option<togi_journal::CampaignCharge>,
    pub mapping: Option<togi_journal::UnitMapping>,
}

/// A permanent stop, explicitly journaled or rederived from CPU evidence and applied ancestry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stop {
    pub unit: UnitId,
    pub witness: Provenance,
    pub last_clean: TargetOffset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Veto {
    pub event: Identity,
    pub profile: ObservedProfile,
    pub witness: Provenance,
}

/// Recovered facts, not a carried current profile, certificate, edge or statistical cap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct History {
    pub campaign: Option<togi_journal::CampaignId>,
    pub charges: Vec<Charge>,
    pub stops: BTreeMap<UnitId, Stop>,
    pub vetoes: Vec<Veto>,
    pub applications: Vec<(Provenance, DayApplied)>,
}

/// Incomplete history must block new nonzero exploration, never imply unused allowance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reconstruction {
    Complete(History),
    Unreconstructible { reason: String },
}

impl Reconstruction {
    #[must_use]
    pub fn history(&self) -> CampaignHistory {
        match self {
            Self::Complete(_) => CampaignHistory::Complete {},
            Self::Unreconstructible { reason } => CampaignHistory::Unreconstructible {
                reason: reason.clone(),
            },
        }
    }
}

/// Decode the original payload strictly, including unknown-field checks.
/// The synthetic envelope adds no policy facts or local cause references.
pub fn decode(record: &DayImported) -> Result<Event, togi_journal::DecodeError> {
    let boot = serde_json::to_string(&record.boot).map_err(|source| {
        togi_journal::DecodeError::Envelope {
            kind: Some(record.original_kind),
            source,
        }
    })?;
    let envelope = format!(
        "{{\"seq\":{},\"time\":\"{}\",\"boot\":{},\"kind\":\"{}\",\"msg\":\"\",\"data\":{}}}",
        record.original.seq,
        record.time,
        boot,
        record.original_kind.as_str(),
        record.raw,
    );
    Event::decode(&envelope)
}

/// Reuse only observations whose frozen content, physical topology and placement still agree.
#[must_use]
pub fn compatible_night(
    observed: &togi_journal::ExposureObserved,
    source: &[togi_machine::CoreInfo],
    current: &[togi_machine::CoreInfo],
    firmware: &togi_machine::BiosContext,
    epoch: u32,
    contents: &BTreeMap<togi_machine::WorkloadId, togi_machine::ContentId>,
) -> bool {
    if source != current
        || observed.epoch != epoch
        || observed.firmware.first_difference(firmware).is_some()
        || observed
            .class
            .workloads
            .iter()
            .any(|entry| contents.get(&entry.workload.runs()) != Some(&entry.content))
    {
        return false;
    }
    let Ok(topology) = crate::Topology::new(current) else {
        return false;
    };
    if observed.profile.ccd().expand(current).as_ref().ok() != Some(observed.profile.cores())
        || observed.class.cores.is_empty()
        || observed
            .class
            .cores
            .iter()
            .any(|core| !topology.all().contains(core))
        || super::ledger::cell_of_class(&observed.class, &topology, current).is_none()
        || observed.exclusions.iter().any(|exclusion| {
            matches!(
                exclusion,
                togi_journal::CreditExclusion::PlacementWithheld {}
            )
        })
    {
        return false;
    }
    match observed.tested {
        togi_journal::Tested::Exact {} => {
            observed.class.placement == togi_journal::Placement::Exact
        }
        togi_journal::Tested::Isolated { ccd, offset } => {
            observed.class.placement == togi_journal::Placement::Isolated
                && observed.profile.ccd().get(ccd) == Some(offset)
                && observed
                    .profile
                    .ccd()
                    .iter()
                    .all(|(unit, value)| unit == ccd || value == TargetOffset::ZERO)
                && observed
                    .class
                    .cores
                    .iter()
                    .all(|core| topology.ccd_of(core) == Some(ccd))
        }
    }
}

/// Reconstruct a campaign from original-identity records in any order, deduplicating copies.
/// Unknown history and malformed required records fail closed. Raw observations are classified at
/// every prefix, so a lost derived event or a later non-CPU answer cannot waive a historical charge.
#[must_use]
pub fn reconstruct(records: &[DayImported], unknown: &[ImportedRecordRef]) -> Reconstruction {
    match recover(records, unknown) {
        Ok(history) => Reconstruction::Complete(history),
        Err(reason) => Reconstruction::Unreconstructible { reason },
    }
}

fn recover(records: &[DayImported], unknown: &[ImportedRecordRef]) -> Result<History, String> {
    if let Some(record) = unknown.first() {
        return Err(format!(
            "undecodable predecessor history: {} at {}",
            record.kind, record.original.seq
        ));
    }
    let mut ordered: Vec<_> = records.iter().collect();
    ordered.sort_by_key(|record| (record.original.session, record.original.seq));
    let mut held = HashMap::new();
    let mut decoded = Vec::new();
    for record in ordered {
        if let Some(raw) = held.insert(record.original, &record.raw) {
            if raw != &record.raw {
                return Err("conflicting copies of predecessor history".into());
            }
            continue;
        }
        let event = decode(record)
            .map_err(|error| format!("undecodable {}: {error}", record.original_kind))?;
        decoded.push((record.original, event));
    }
    let mut topologies = HashMap::new();
    for (original, event) in &decoded {
        if let Payload::SessionStart(start) = &event.payload {
            if start.session != original.session {
                return Err("predecessor topology has the wrong original session".into());
            }
            let topology = crate::Topology::new(&start.cores)
                .map_err(|error| format!("cannot reconstruct predecessor topology: {error}"))?;
            topologies.insert(start.session, topology.all());
        }
    }
    let mut identities = HashMap::new();
    for (original, event) in &decoded {
        if let Payload::DayRecord(record) = &event.payload {
            let identity = match &record.boot {
                togi_journal::CrashedBoot::UncleanEnd { boot } => Identity::Boot(boot.clone()),
                togi_journal::CrashedBoot::Unobserved { at } => Identity::Unobserved(*at),
            };
            identities.insert(*original, identity);
        }
        let id = if let Payload::DayEvent(classified) = &event.payload {
            Some(&classified.event)
        } else if let Payload::CampaignCharge(charged) = &event.payload {
            Some(&charged.event)
        } else {
            None
        };
        if let Some(DayEventId::Requalification { night }) = id {
            identities.insert(
                Provenance {
                    session: original.session,
                    seq: night.0,
                },
                Identity::Requalification {
                    session: original.session,
                    night: *night,
                },
            );
        }
    }
    // A failed re-qualification of a nonzero current profile is a CPU-suspect event even when the
    // session ended before its derived `day.event` was written. The raw qualification and
    // exposure records are not enough to recharge it faithfully here, so it fails closed.
    for (original, event) in &decoded {
        if let Payload::NightQualification(qualification) = &event.payload
            && qualification.purpose == QualificationPurpose::Requalification
            && matches!(qualification.state, QualificationState::Failed { .. })
            && qualification
                .profile
                .cores()
                .iter()
                .any(|(_, offset)| offset.get() != 0)
            && !decoded.iter().any(|(other, derived)| {
                other.session == original.session
                    && matches!(&derived.payload, Payload::DayEvent(classified)
                        if matches!(classified.event, DayEventId::Requalification { .. })
                            && classified.witnesses.contains(&original.seq))
            })
        {
            return Err(
                "a failed current-profile requalification has no derived campaign event".into(),
            );
        }
    }
    let mut history = History::default();
    let mut latest = HashMap::new();
    let mut raw = State::new(Params::SPEC);
    let mut steps = HashMap::new();
    let mut climbs = HashMap::new();
    let mut recorded_consumed = 0;
    for (index, (original, event)) in decoded.iter().enumerate() {
        validate_profile(original.session, &event.payload, &topologies)?;
        match &event.payload {
            Payload::CampaignStart(start) => {
                if history
                    .campaign
                    .is_some_and(|campaign| campaign != start.campaign)
                {
                    return Err("predecessor history contains conflicting campaigns".into());
                }
                history.campaign = Some(start.campaign);
                if let CampaignHistory::Unreconstructible { reason } = &start.history {
                    return Err(reason.clone());
                }
            }
            Payload::SuccessorImported(imported) => {
                if let CampaignHistory::Unreconstructible { reason } = &imported.campaign {
                    return Err(reason.clone());
                }
            }
            Payload::DayStep(step) => {
                steps.insert((original.session, step.decision), step);
            }
            Payload::ClimbDecision(decision) => {
                climbs.insert(
                    (
                        original.session,
                        togi_journal::DecisionId(original.seq.get()),
                    ),
                    decision,
                );
            }
            Payload::DayApplied(applied) => {
                if let ApplyCause::Step { decision } = applied.cause {
                    if let Some(step) = steps.get(&(original.session, decision)) {
                        if step.proposed != applied.target {
                            return Err("successful application disagrees with its decision".into());
                        }
                    } else if let Some(step) = climbs.get(&(original.session, decision)) {
                        // A stock-only receipt for a nonzero proposal is a verified reset, not the
                        // step; the live fold observes it without a transition. Any other
                        // disagreement is a nonzero application with no decision behind it.
                        if &step.proposed.cores != applied.target.cores()
                            && applied
                                .target
                                .cores()
                                .iter()
                                .any(|(_, offset)| offset.get() != 0)
                        {
                            return Err(
                                "successful application disagrees with its climb decision".into()
                            );
                        }
                    } else {
                        return Err("successful application is missing its decision history".into());
                    }
                }
                if applied.target.cores().is_empty()
                    || applied.readbacks.len() != applied.target.cores().len()
                    || applied.target.cores().iter().any(|(core, offset)| {
                        applied.readbacks.get(core).map(|value| value.0) != Some(offset.get())
                    })
                {
                    return Err("successful application has inconsistent readbacks".into());
                }
                history.applications.push((*original, applied.clone()));
            }
            Payload::DayEvent(event) => {
                let identity = identity(&event.event, original.session, &identities);
                latest.insert(identity.clone(), event.classification);
                if matches!(event.classification, Classification::CpuSuspect { .. })
                    && nonzero(&event.profile)
                {
                    charge(
                        &mut history,
                        identity,
                        Vec::new(),
                        event.profile.clone(),
                        matches!(
                            event.classification,
                            Classification::CpuSuspect {
                                certainty: togi_journal::Certainty::Provisional
                            }
                        ),
                        *original,
                    );
                }
            }
            Payload::CampaignCharge(charged) => {
                recorded_consumed = recorded_consumed.max(charged.consumed);
                if charged.remaining != 2_u32.saturating_sub(charged.consumed)
                    || charged.exhausted != (charged.consumed >= 2)
                {
                    return Err("inconsistent predecessor campaign charge".into());
                }
                let id = identity(&charged.event, original.session, &identities);
                let aliases = charged
                    .aliases
                    .iter()
                    .map(|alias| identity(alias, original.session, &identities))
                    .collect();
                let profile = ObservedProfile::PerCore {
                    cores: charged
                        .profile
                        .cores
                        .iter()
                        .map(|(core, offset)| (core, offset.observed()))
                        .collect(),
                };
                if nonzero(&profile) {
                    charge(
                        &mut history,
                        id.clone(),
                        aliases,
                        profile,
                        charged.provisional,
                        *original,
                    );
                    if let Some(entry) = history
                        .charges
                        .iter_mut()
                        .find(|entry| entry.event == id || entry.aliases.contains(&id))
                    {
                        entry.phase = charged.phase;
                        entry.unit_phase = charged.unit_phase;
                        entry.receipt = Some(charged.clone());
                        entry.mapping = Some(charged.profile.mapping.clone());
                    }
                }
            }
            Payload::CampaignReclassified(revised) => {
                latest.insert(
                    identity(&revised.event, original.session, &identities),
                    revised.to,
                );
            }
            Payload::UnitStop(stop) => {
                history.stops.entry(stop.unit).or_insert(Stop {
                    unit: stop.unit,
                    witness: *original,
                    last_clean: stop.last_clean,
                });
            }
            Payload::DayVeto(veto) => {
                let id = identity(&veto.event, original.session, &identities);
                history.vetoes.retain(|held| held.event != id);
                if veto.state == VetoState::Active {
                    history.vetoes.push(Veto {
                        event: id,
                        profile: veto.profile.clone(),
                        witness: *original,
                    });
                }
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
            | Payload::DeadEnd(_)
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
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::TaskStart(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayPause(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::UnitMapping(_)
            | Payload::UnitEdge(_)
            | Payload::FirmwareRestoration(_) => {}
        }
        if raw_kind(event.kind()) {
            let mut event = event.clone();
            event.seq = Seq::new(
                u64::try_from(index)
                    .map_err(|_| "too many predecessor records")?
                    .checked_add(1)
                    .ok_or("too many predecessor records")?,
            )
            .ok_or("invalid predecessor sequence")?;
            raw.fold(&event)
                .map_err(|error| format!("cannot reconstruct raw predecessor history: {error}"))?;
            if matches!(
                event.kind(),
                EventKind::DayCheckpoint | EventKind::DayCleanStop
            ) {
                continue;
            }
            let view = raw
                .day_view()
                .map_err(|error| format!("cannot classify predecessor history: {error}"))?;
            for event in view.events {
                let id = match event.id {
                    EventId::Boot(boot) => Identity::Boot(boot),
                    EventId::Imported(original) => Identity::Record(original),
                    EventId::Unobserved(at) => Identity::Unobserved(at),
                    EventId::Requalification(seq) => Identity::Requalification {
                        session: original.session,
                        night: togi_journal::NightId(seq),
                    },
                };
                latest.insert(
                    id.clone(),
                    if event.class.is_cpu_suspect() {
                        Classification::CpuSuspect {
                            certainty: togi_journal::Certainty::Definite,
                        }
                    } else {
                        Classification::NonCpu {}
                    },
                );
                if event.class.is_cpu_suspect() {
                    charge(
                        &mut history,
                        id,
                        Vec::new(),
                        ObservedProfile::PerCore {
                            cores: *event.profile.offsets(),
                        },
                        matches!(
                            event.class,
                            crate::day::Classification::ProvisionalCpuSuspect(_)
                        ),
                        *original,
                    );
                }
            }
        }
    }
    // Merge bridge aliases after all records have been read (not just adjacent copies).
    merge_aliases(&mut history.charges);
    if u32::try_from(history.charges.len()).is_ok_and(|count| recorded_consumed > count) {
        return Err("missing predecessor campaign charges".into());
    }
    for charged in &mut history.charges {
        let (ObservedProfile::Ccd { cores, .. } | ObservedProfile::PerCore { cores }) =
            &charged.profile;
        if cores.is_empty()
            || cores
                .iter()
                .any(|(_, offset)| TargetOffset::new(offset.0).is_err())
        {
            return Err("campaign event is missing a valid observed profile".into());
        }
        charged.reclassified = std::iter::once(&charged.event)
            .chain(&charged.aliases)
            .any(|id| matches!(latest.get(id), Some(Classification::NonCpu {})));
        if !charged.reclassified
            && !history
                .vetoes
                .iter()
                .any(|veto| veto.event == charged.event)
        {
            history.vetoes.push(Veto {
                event: charged.event.clone(),
                profile: charged.profile.clone(),
                witness: charged.witnesses[0],
            });
        }
    }
    history
        .vetoes
        .retain(|veto| !matches!(latest.get(&veto.event), Some(Classification::NonCpu {})));
    reconstruct_stops(&mut history, &decoded)?;
    for (original, applied) in &history.applications {
        if let ApplyCause::Step { decision } = applied.cause
            && let Some(step) = climbs.get(&(original.session, decision))
            && let togi_journal::ClimbAction::Retreat { units, .. } = &step.decision
            && units.iter().any(|unit| !history.stops.contains_key(unit))
        {
            return Err(
                "successful predecessor retreat is missing its permanent-stop evidence".into(),
            );
        }
    }
    Ok(history)
}

/// Statistical retreats are application history, never permanent successor stops.
fn reconstruct_stops(history: &mut History, records: &[(Provenance, Event)]) -> Result<(), String> {
    use crate::climb::Cursor;
    let mut consumed = BTreeSet::<Cursor>::new();
    let mut tails = Vec::<togi_machine::PerCore<togi_machine::ObservedOffset>>::new();
    for charged in &mut history.charges {
        let witness = charged
            .witnesses
            .first()
            .copied()
            .ok_or("campaign charge has no witness")?;
        let (_, witnessed) = records
            .iter()
            .find(|(original, _)| *original == witness)
            .ok_or("campaign event witness is missing")?;
        let at = if let Payload::DayRecord(record) = &witnessed.payload {
            match record.boot {
                togi_journal::CrashedBoot::Unobserved { at } => at,
                togi_journal::CrashedBoot::UncleanEnd { .. } => witnessed.time,
            }
        } else if let Payload::DayEvent(event) = &witnessed.payload {
            event.from
        } else {
            witnessed.time
        };
        let (ObservedProfile::Ccd { cores, .. } | ObservedProfile::PerCore { cores }) =
            &charged.profile;
        let (last, _) = records.iter().rev().find(|(original, event)| {
            original.session == witness.session && original.seq < witness.seq && event.time <= at
                && matches!(&event.payload, Payload::DayApplied(applied)
                    if applied.target.cores().iter().all(|(core, offset)| cores.get(core) == Some(offset.observed())))
        }).ok_or("CPU-suspect event is missing successful applied-step ancestry")?;
        let applied = applied_history(records, *last)?;
        if charged.mapping.is_none() {
            charged.unit_phase = match applied.units().phase() {
                crate::unit::Phase::Coarse => togi_journal::UnitPhase::Coarse,
                crate::unit::Phase::Core => togi_journal::UnitPhase::Core,
            };
            charged.mapping = Some(togi_journal::UnitMapping {
                phase: charged.unit_phase,
                units: applied
                    .units()
                    .iter()
                    .map(|unit| togi_journal::UnitMembership {
                        unit: unit.id,
                        cores: unit.cores,
                        parent: unit.parent,
                    })
                    .collect(),
            });
        }
        let all = tails.iter().any(|tail| tail == cores);
        let choices: Vec<_> = applied
            .current()
            .iter()
            .filter_map(|(unit, now)| {
                applied
                    .newest(unit, now, &consumed)
                    .map(|link| (unit, now, link))
            })
            .collect();
        let newest = choices
            .iter()
            .map(|(_, _, link)| link.transition.application)
            .max();
        let mut target = applied.current().clone();
        for (unit, now, link) in choices {
            if all || Some(link.transition.application) == newest {
                consumed.insert(Cursor {
                    unit,
                    transition: link.transition,
                });
                let clean = now.max(link.before);
                target = target.with(unit, clean);
                history.stops.entry(unit).or_insert(Stop {
                    unit,
                    witness,
                    last_clean: clean,
                });
            }
        }
        let tail = target
            .expand(applied.units())
            .map_err(|error| error.to_string())?
            .iter()
            .map(|(core, offset)| (core, offset.observed()))
            .collect();
        tails.push(tail);
    }
    Ok(())
}

fn applied_history(
    records: &[(Provenance, Event)],
    last: Provenance,
) -> Result<crate::climb::ClimbHistory, String> {
    use crate::climb::{
        ApplicationId, Ceilings, ClimbAction, ClimbDecision, ClimbHistory, Proposal,
        RestorationKind, RestorationProposal, Retreat, SavedProfile,
    };
    use crate::unit::{UnitProfile, Units};
    let start = records
        .iter()
        .find_map(|(original, event)| {
            if original.session == last.session
                && let Payload::SessionStart(start) = &event.payload
            {
                Some(start)
            } else {
                None
            }
        })
        .ok_or("missing applied-step topology")?;
    let topology = crate::Topology::new(&start.cores).map_err(|error| error.to_string())?;
    let mut history = ClimbHistory::stock(Units::coarse(&topology));
    for (index, (original, event)) in records.iter().enumerate() {
        if original.session != last.session || original.seq > last.seq {
            continue;
        }
        let Payload::DayApplied(applied) = &event.payload else {
            continue;
        };
        let step = if let ApplyCause::Step { decision } = applied.cause {
            Some(records.iter().find_map(|(source, event)| {
                if source.session != last.session || source.seq >= original.seq { return None; }
                if matches!(&event.payload, Payload::DayStep(step) if step.decision == decision)
                    || matches!(&event.payload, Payload::ClimbDecision(_) if source.seq.get() == decision.0)
                { Some(&event.payload) } else { None }
            }).ok_or("missing successful applied-step decision")?)
        } else {
            None
        };
        let step = step.filter(|payload| {
            !matches!(payload, Payload::ClimbDecision(decision)
                if &decision.proposed.cores != applied.target.cores())
        });
        if let Some(Payload::ClimbDecision(step)) = step {
            let units = match step.proposed.mapping.phase {
                togi_journal::UnitPhase::Coarse => Units::coarse(&topology),
                togi_journal::UnitPhase::Core => Units::cores(&topology),
            };
            if &units != history.units() {
                let profile = SavedProfile {
                    units: history.units().clone(),
                    profile: history.current().clone(),
                }
                .for_units(&units)
                .map_err(|error| error.to_string())?;
                history
                    .refine(units, profile)
                    .map_err(|error| error.to_string())?;
            }
        }
        let physical = history
            .current()
            .expand(history.units())
            .map_err(|error| error.to_string())?;
        let expected = match step {
            Some(Payload::DayStep(step)) => Some(step.old.cores()),
            Some(Payload::ClimbDecision(step)) => Some(&step.before.cores),
            _ => None,
        };
        if expected.is_some_and(|expected| expected != &physical) {
            return Err("successful application has missing predecessor step ancestry".into());
        }
        let target = UnitProfile::try_from(
            history
                .units()
                .iter()
                .map(|unit| {
                    let first = unit
                        .cores
                        .first()
                        .and_then(|core| applied.target.cores().get(core))
                        .ok_or("applied history is missing a unit")?;
                    if unit
                        .cores
                        .iter()
                        .any(|core| applied.target.cores().get(core) != Some(first))
                    {
                        return Err("applied history disagrees with its unit mapping");
                    }
                    Ok((unit.id, first))
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
        .map_err(|error| error.to_string())?;
        if target == *history.current() {
            continue;
        }
        let promotion = match step {
            Some(Payload::DayStep(step)) => match step.step {
                StepKind::Promote { changed } => Some(UnitId::Ccd(changed)),
                StepKind::Hold { .. }
                | StepKind::Retreat { .. }
                | StepKind::FirmwareZero { .. } => None,
            },
            Some(Payload::ClimbDecision(step)) => match step.decision {
                togi_journal::ClimbAction::Promote { unit, .. } => Some(unit),
                togi_journal::ClimbAction::Hold { .. }
                | togi_journal::ClimbAction::Retreat { .. }
                | togi_journal::ClimbAction::Restore { .. }
                | togi_journal::ClimbAction::FirmwareZero {} => None,
            },
            _ => None,
        };
        let action = if let Some(unit) = promotion {
            let from = history
                .current()
                .get(unit)
                .ok_or("promotion has no old unit depth")?;
            let to = target.get(unit).ok_or("promotion has no new unit depth")?;
            if to >= from
                || target.iter().any(|(other, value)| {
                    other != unit && history.current().get(other) != Some(value)
                })
            {
                return Err("successful promotion does not match its changed unit".into());
            }
            ClimbAction::Promote(Proposal {
                unit,
                from,
                to,
                step: from.get() - to.get(),
                remaining: from.get() - to.get(),
                profile: target,
            })
        } else if matches!(step, Some(Payload::ClimbDecision(step)) if matches!(step.decision, togi_journal::ClimbAction::Restore { .. }))
        {
            ClimbAction::Restore(RestorationProposal {
                kind: RestorationKind::Firmware,
                profile: target,
            })
        } else {
            ClimbAction::Retreat(Retreat {
                profile: target.clone(),
                wanted: target,
                events: Vec::new(),
                cursors: Vec::new(),
                stopped: Vec::new(),
                adverse_units: Vec::new(),
            })
        };
        let decision = ClimbDecision {
            before: history.current().clone(),
            ceilings: Ceilings::default(),
            action,
        };
        let id = ApplicationId::new(
            u64::try_from(index)
                .map_err(|_| "too many applications")?
                .checked_add(1)
                .ok_or("too many applications")?,
        )
        .ok_or("invalid application identity")?;
        history
            .apply(id, event.time, &decision)
            .map_err(|error| error.to_string())?;
    }
    Ok(history)
}

fn validate_profile(
    session: SessionId,
    payload: &Payload,
    topologies: &HashMap<SessionId, togi_machine::CoreSet>,
) -> Result<(), String> {
    let observed = if let Payload::DayBootStart(start) = payload {
        Some(&start.offsets)
    } else if let Payload::DayUncleanEnd(end) = payload {
        Some(&end.offsets)
    } else if let Payload::DayFault(fault) = payload {
        Some(&fault.offsets)
    } else if let Payload::DayRecord(record) = payload {
        Some(record.offsets.get())
    } else if let Payload::DayAnswer(answer) = payload {
        Some(&answer.offsets)
    } else if let Payload::DayOffsetsChanged(changed) = payload {
        Some(&changed.offsets)
    } else if let Payload::DayEvent(event) = payload {
        let (ObservedProfile::Ccd { cores, .. } | ObservedProfile::PerCore { cores }) =
            &event.profile;
        Some(cores)
    } else {
        None
    };
    if let Some(observed) = observed {
        full_vector(session, observed, topologies)?;
    }
    if let Payload::DayApplied(applied) = payload {
        full_vector(session, applied.target.cores(), topologies)?;
    }
    if let Payload::CampaignCharge(charge) = payload {
        full_vector(session, &charge.profile.cores, topologies)?;
    }
    Ok(())
}

fn full_vector<T>(
    session: SessionId,
    offsets: &togi_machine::PerCore<T>,
    topologies: &HashMap<SessionId, togi_machine::CoreSet>,
) -> Result<(), String> {
    let cores = topologies
        .get(&session)
        .ok_or("missing predecessor campaign topology")?;
    if offsets.len() != cores.len() || offsets.iter_ref().any(|(core, _)| !cores.contains(core)) {
        return Err("incomplete predecessor campaign profile".into());
    }
    Ok(())
}

fn raw_kind(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::DayBootStart
            | EventKind::DayCheckpoint
            | EventKind::DayOffsetsChanged
            | EventKind::DayCleanStop
            | EventKind::DayUncleanEnd
            | EventKind::DayAnswer
            | EventKind::DayRecord
            | EventKind::DayFault
            | EventKind::DayPause
            | EventKind::Mce
    )
}

fn identity(
    id: &DayEventId,
    session: SessionId,
    records: &HashMap<Provenance, Identity>,
) -> Identity {
    match id {
        DayEventId::Boot { boot } => Identity::Boot(boot.clone()),
        DayEventId::Record { session, seq } => {
            let original = Provenance {
                session: *session,
                seq: *seq,
            };
            records
                .get(&original)
                .cloned()
                .unwrap_or(Identity::Record(original))
        }
        DayEventId::Requalification { night } => Identity::Requalification {
            session,
            night: *night,
        },
    }
}

fn nonzero(profile: &ObservedProfile) -> bool {
    let (ObservedProfile::Ccd { cores, .. } | ObservedProfile::PerCore { cores }) = profile;
    cores.iter().any(|(_, offset)| offset.0 != 0)
}

fn charge(
    history: &mut History,
    event: Identity,
    aliases: Vec<Identity>,
    profile: ObservedProfile,
    provisional: bool,
    witness: Provenance,
) {
    if let Some(held) = history
        .charges
        .iter_mut()
        .find(|held| held.event == event || held.aliases.contains(&event))
    {
        held.provisional |= provisional;
        held.profile = profile;
        if !held.witnesses.contains(&witness) {
            held.witnesses.push(witness);
        }
        for alias in aliases {
            if alias != held.event && !held.aliases.contains(&alias) {
                held.aliases.push(alias);
            }
        }
    } else {
        history.charges.push(Charge {
            event,
            aliases,
            profile,
            provisional,
            reclassified: false,
            witnesses: vec![witness],
            phase: togi_journal::CampaignEventPhase::Learning,
            unit_phase: togi_journal::UnitPhase::Coarse,
            receipt: None,
            mapping: None,
        });
    }
}

fn merge_aliases(charges: &mut Vec<Charge>) {
    let mut index = 0;
    while index < charges.len() {
        let mut seen: Vec<_> = std::iter::once(charges[index].event.clone())
            .chain(charges[index].aliases.iter().cloned())
            .collect();
        let mut other = index + 1;
        while other < charges.len() {
            if std::iter::once(&charges[other].event)
                .chain(&charges[other].aliases)
                .any(|id| seen.contains(id))
            {
                let merged = charges.remove(other);
                if !seen.contains(&merged.event) {
                    seen.push(merged.event);
                }
                for alias in merged.aliases {
                    if !seen.contains(&alias) {
                        seen.push(alias);
                    }
                }
                charges[index].provisional |= merged.provisional;
                charges[index].witnesses.extend(merged.witnesses);
                if let Some(receipt) = merged.receipt {
                    charges[index].phase = receipt.phase;
                    charges[index].unit_phase = receipt.unit_phase;
                    charges[index].mapping = Some(receipt.profile.mapping.clone());
                    charges[index].receipt = Some(receipt);
                }
                // A bridge can connect a previously disjoint record.
                other = index + 1;
            } else {
                other += 1;
            }
        }
        seen.retain(|id| *id != charges[index].event);
        charges[index].aliases = seen;
        charges[index]
            .witnesses
            .sort_by_key(|witness| (witness.session, witness.seq));
        charges[index].witnesses.dedup();
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(kind: &str, seq: u64, data: &serde_json::Value) -> DayImported {
        DayImported {
            original: Provenance {
                session: "20261001T000000Z".parse().unwrap(),
                seq: Seq::new(seq + 10).unwrap(),
            },
            original_kind: kind.parse().unwrap(),
            raw: data.to_string(),
            time: "2026-10-01T00:00:00Z".parse().unwrap(),
            boot: BootId::from("boot".to_owned()),
            build: None,
            firmware: None,
            profile: None,
            class: togi_journal::ImportClass::Manual,
            missing: Vec::new(),
        }
    }

    fn header() -> DayImported {
        let start = togi_journal::SessionStart {
            session: "20261001T000000Z".parse().unwrap(),
            build: togi_journal::Build {
                version: "0.1.0".into(),
                rev: None,
                ruleset: 2,
                schema: togi_journal::SCHEMA,
                fixes: togi_journal::FixStamp::none(),
                evidence: 1,
            },
            cores: vec![togi_machine::CoreInfo {
                core: togi_machine::CoreId::new(0).unwrap(),
                ccd: togi_machine::CcdId::try_from(0_u8).unwrap(),
                cpus: vec![
                    togi_machine::CpuId::new(0).unwrap(),
                    togi_machine::CpuId::new(1).unwrap(),
                ],
            }],
        };
        let mut header = record("session.start", 0, &serde_json::to_value(start).unwrap());
        header.original.seq = Seq::FIRST;
        header
    }

    fn classified(seq: u64, classification: &serde_json::Value) -> DayImported {
        record(
            "day.event",
            seq,
            &json!({
                "event": {"kind":"boot","boot":"crashed"},
                "profile":{"kind":"per_core","cores":{"0":-5}},
                "from":"2026-10-01T00:00:00Z","to":"2026-10-01T00:00:00Z",
                "witnesses":[1],"classification":classification,
                "resolution":{"kind":"unresolved"},"reason":"raw witnesses",
                "reproduction":"pending"
            }),
        )
    }

    fn complete(records: &[DayImported]) -> History {
        let records: Vec<_> = initial_application()
            .into_iter()
            .chain(records.iter().cloned())
            .collect();
        match reconstruct(&records, &[]) {
            Reconstruction::Complete(history) => history,
            Reconstruction::Unreconstructible { reason } => panic!("{reason}"),
        }
    }

    fn initial_application() -> Vec<DayImported> {
        let mut step = record(
            "day.step",
            0,
            &json!({
                "decision":100,"cold_boot":"boot","old":{"ccd":{"0":0},"cores":{"0":0}},
                "proposed":{"ccd":{"0":-5},"cores":{"0":-5}},
                "step":{"kind":"promote","changed":0},
                "hours":[],"hold_s":0,"causes":{},"reason":"actual promotion"
            }),
        );
        let mut applied = record(
            "day.applied",
            0,
            &json!({
                "application":1,"cause":{"kind":"step","decision":100},
                "target":{"ccd":{"0":-5},"cores":{"0":-5}},
                "readbacks":{"0":-5},"transactions":[1],"boot":"boot","mono_ns":0
            }),
        );
        step.original.seq = Seq::new(2).unwrap();
        applied.original.seq = Seq::new(3).unwrap();
        step.time = "2026-09-01T00:00:00Z".parse().unwrap();
        applied.time = step.time;
        vec![header(), step, applied]
    }

    #[test]
    fn provisional_charges_survive_reclassification_and_import_copies() {
        let provisional = classified(1, &json!({"kind":"cpu_suspect","certainty":"provisional"}));
        let revised = classified(2, &json!({"kind":"non_cpu"}));
        let history = complete(&[provisional.clone(), revised, provisional]);
        assert_eq!(history.charges.len(), 1);
        assert!(history.charges[0].provisional);
        assert!(history.charges[0].reclassified);
        assert!(history.vetoes.is_empty());
        // The reclassification lifts the veto but never the permanent stop the event caused.
        assert_eq!(history.stops.len(), 1);
        assert_eq!(
            history.stops.values().next().map(|stop| stop.last_clean),
            Some(TargetOffset::ZERO)
        );
    }

    #[test]
    fn copied_manual_witnesses_of_one_crash_consume_one_event_without_a_derived_commitment() {
        let first = record(
            "day.record",
            1,
            &json!({
                "boot":{"kind":"unobserved","at":"2026-09-30T23:00:00Z"},
                "offsets":{"0":-5},"source":{"kind":"entered","at":"2026-10-01T00:00:00Z"},
                "running":"idle"
            }),
        );
        let mut second = first.clone();
        second.original.seq = Seq::new(13).unwrap();
        let history = complete(&[second, first.clone(), first]);
        assert_eq!(history.charges.len(), 1);
        assert_eq!(history.vetoes.len(), 1);
    }

    #[test]
    fn events_at_stock_do_not_consume_the_campaign_allowance() {
        let mut event = classified(1, &json!({"kind":"cpu_suspect","certainty":"definite"}));
        let mut data: serde_json::Value = serde_json::from_str(&event.raw).unwrap();
        data["profile"]["cores"]["0"] = json!(0);
        event.raw = data.to_string();
        assert!(complete(&[event]).charges.is_empty());
    }

    #[test]
    fn undecodable_required_campaign_history_is_not_zero_consumption() {
        let malformed = record("day.applied", 1, &json!({}));
        assert!(matches!(
            reconstruct(&[malformed], &[]),
            Reconstruction::Unreconstructible { .. }
        ));
    }

    #[test]
    fn missing_successful_application_ancestry_is_unreconstructible() {
        let applied = record(
            "day.applied",
            1,
            &json!({
                "application":1,"cause":{"kind":"step","decision":1},
                "target":{"ccd":{"0":-5},"cores":{"0":-5}},
                "readbacks":{"0":-5},"transactions":[1],"boot":"boot","mono_ns":0
            }),
        );
        assert!(
            matches!(reconstruct(&[header(), applied], &[]), Reconstruction::Unreconstructible { reason } if reason.contains("decision"))
        );
    }

    #[test]
    fn a_statistical_cap_or_completion_retreat_does_not_become_a_successor_stop() {
        let step = record(
            "day.step",
            1,
            &json!({
                "decision":1,"cold_boot":"boot","old":{"ccd":{"0":-5},"cores":{"0":-5}},
                "proposed":{"ccd":{"0":0},"cores":{"0":0}},
                "step":{"kind":"retreat","changed":[0],"cause":{"kind":"completion"}},
                "hours":[],"hold_s":0,"causes":{},"reason":"backstep"
            }),
        );
        let applied = record(
            "day.applied",
            2,
            &json!({
                "application":2,"cause":{"kind":"step","decision":1},
                "target":{"ccd":{"0":0},"cores":{"0":0}},
                "readbacks":{"0":0},"transactions":[1],"boot":"boot","mono_ns":0
            }),
        );
        let history = complete(&[applied, step]);
        assert!(history.stops.is_empty());
        assert_eq!(history.applications.len(), 2);
    }

    #[test]
    fn a_raw_provisional_end_is_charged_even_when_its_derived_commitment_was_lost() {
        let end = record(
            "day.unclean_end",
            1,
            &json!({
                "boot":"crashed","offsets":{"0":-5},"reset_reason":{"raw":[],"supported":true}
            }),
        );
        let answer = record(
            "day.answer",
            2,
            &json!({
                "boot":"crashed","offsets":{"0":-5},"answer":"power_off","uid":1000
            }),
        );
        let history = complete(&[end, answer]);
        assert_eq!(history.charges.len(), 1);
        assert!(history.charges[0].provisional);
        assert!(history.charges[0].reclassified);
        assert!(history.vetoes.is_empty());
    }

    #[test]
    fn missing_campaign_topology_is_unreconstructible() {
        let event = classified(1, &json!({"kind":"cpu_suspect","certainty":"definite"}));
        assert!(
            matches!(reconstruct(&[event], &[]), Reconstruction::Unreconstructible { reason } if reason.contains("topology"))
        );
    }

    #[test]
    fn a_cpu_event_reconstructs_a_stop_without_a_successful_retreat() {
        let event = classified(1, &json!({"kind":"cpu_suspect","certainty":"provisional"}));
        let witness = event.original;
        let history = complete(&[event]);
        let stop = history.stops.values().next().unwrap();
        assert_eq!(stop.witness, witness);
        assert_eq!(stop.last_clean, TargetOffset::ZERO);
    }

    #[test]
    fn a_nonzero_event_without_successful_step_history_is_unreconstructible() {
        let event = classified(1, &json!({"kind":"cpu_suspect","certainty":"definite"}));
        assert!(matches!(reconstruct(&[header(), event], &[]),
            Reconstruction::Unreconstructible { reason } if reason.contains("applied-step ancestry")));
    }
}
