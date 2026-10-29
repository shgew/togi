//! Reconstruct irreversible campaign facts without carrying predecessor permission.
//!
//! Inputs retain their original sequence namespace. Applications are history only: callers must
//! start the successor at verified stock, and recompute edges and qualification from observations.

use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use togi_journal::{
    ApplyCause, CampaignHistory, Classification, DayApplied, DayEventId, DayImported,
    Event, EventKind, ImportedRecordRef, ObservedProfile, Payload, Provenance, Seq,
    SessionId, StepKind, UnitId, VetoState,
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
    Requalification { session: SessionId, night: togi_journal::NightId },
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
}

/// A permanent stop, either explicitly journaled or recovered from a successful backstep.
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
            Self::Unreconstructible { reason } => CampaignHistory::Unreconstructible { reason: reason.clone() },
        }
    }
}

/// Decode the original payload strictly, including unknown-field checks.
/// The synthetic envelope adds no policy facts or local cause references.
pub fn decode(record: &DayImported) -> Result<Event, togi_journal::DecodeError> {
    let envelope = format!(
        "{{\"seq\":{},\"time\":\"{}\",\"boot\":{},\"kind\":\"{}\",\"msg\":\"\",\"data\":{}}}",
        record.original.seq, record.time,
        serde_json::to_string(&record.boot).unwrap_or_default(),
        record.original_kind.as_str(), record.raw,
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
    if source != current || observed.epoch != epoch
        || observed.firmware.first_difference(firmware).is_some()
        || observed.class.workloads.iter().any(|entry| contents.get(&entry.workload.runs()) != Some(&entry.content))
    {
        return false;
    }
    let Ok(topology) = crate::Topology::new(current) else { return false; };
    if observed.profile.ccd().expand(current).as_ref().ok() != Some(observed.profile.cores())
        || observed.class.cores.is_empty()
        || observed.class.cores.iter().any(|core| !topology.all().contains(core))
        || super::ledger::cell_of_class(&observed.class, &topology, current).is_none()
        || observed.exclusions.iter().any(|exclusion| matches!(exclusion, togi_journal::CreditExclusion::PlacementWithheld {}))
    {
        return false;
    }
    match observed.tested {
        togi_journal::Tested::Exact {} => observed.class.placement == togi_journal::Placement::Exact,
        togi_journal::Tested::Isolated { ccd, offset } => {
            observed.class.placement == togi_journal::Placement::Isolated
                && observed.profile.ccd().get(ccd) == Some(offset)
                && observed.profile.ccd().iter().all(|(unit, value)| unit == ccd || value == TargetOffset::ZERO)
                && observed.class.cores.iter().all(|core| topology.ccd_of(core) == Some(ccd))
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
        return Err(format!("undecodable predecessor history: {} at {}", record.kind, record.original.seq));
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
        let event = decode(record).map_err(|error| format!("undecodable {}: {error}", record.original_kind))?;
        decoded.push((record.original, event));
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
    }
    let mut history = History::default();
    let mut latest = HashMap::new();
    let mut raw = State::new(Params::SPEC);
    let mut steps = HashMap::new();
    for (index, (original, event)) in decoded.iter().enumerate() {
        match &event.payload {
            Payload::CampaignStart(start) => {
                if history.campaign.is_some_and(|campaign| campaign != start.campaign) {
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
            Payload::DayStep(step) => { steps.insert((original.session, step.decision), step); }
            Payload::DayApplied(applied) => {
                if let ApplyCause::Step { decision } = applied.cause {
                    let step = steps.get(&(original.session, decision))
                        .ok_or_else(|| "successful application is missing its decision history".to_owned())?;
                    if step.proposed != applied.target {
                        return Err("successful application disagrees with its decision".into());
                    }
                    if let StepKind::Retreat { changed, .. } = &step.step {
                        for ccd in changed {
                            let unit = UnitId::Ccd(*ccd);
                            let last_clean = applied.target.ccd().get(*ccd)
                                .ok_or_else(|| "backstep is missing its resulting unit depth".to_owned())?;
                            history.stops.entry(unit).or_insert(Stop { unit, witness: *original, last_clean });
                        }
                    }
                }
                if applied.target.cores().iter().any(|(core, offset)| applied.readbacks.get(core).map(|value| value.0) != Some(offset.get())) {
                    return Err("successful application has inconsistent readbacks".into());
                }
                history.applications.push((*original, applied.clone()));
            }
            Payload::DayEvent(event) => {
                let identity = identity(&event.event, original.session, &identities);
                latest.insert(identity.clone(), event.classification);
                if matches!(event.classification, Classification::CpuSuspect { .. }) && nonzero(&event.profile) {
                    charge(&mut history, identity, Vec::new(), event.profile.clone(),
                        matches!(event.classification, Classification::CpuSuspect { certainty: togi_journal::Certainty::Provisional }), *original);
                }
            }
            Payload::CampaignCharge(charged) => {
                let id = identity(&charged.event, original.session, &identities);
                let aliases = charged.aliases.iter().map(|alias| identity(alias, original.session, &identities)).collect();
                let profile = ObservedProfile::PerCore { cores: charged.profile.cores.iter().map(|(core, offset)| (core, offset.observed())).collect() };
                if nonzero(&profile) { charge(&mut history, id, aliases, profile, charged.provisional, *original); }
            }
            Payload::CampaignReclassified(revised) => {
                latest.insert(identity(&revised.event, original.session, &identities), revised.to);
            }
            Payload::UnitStop(stop) => {
                history.stops.entry(stop.unit).or_insert(Stop { unit: stop.unit, witness: *original, last_clean: stop.last_clean });
            }
            Payload::DayVeto(veto) => {
                let id = identity(&veto.event, original.session, &identities);
                history.vetoes.retain(|held| held.event != id);
                if veto.state == VetoState::Active {
                    history.vetoes.push(Veto { event: id, profile: veto.profile.clone(), witness: *original });
                }
            }
            _ => {}
        }
        if raw_kind(event.kind()) {
            let mut event = event.clone();
            event.seq = Seq::new(u64::try_from(index).map_err(|_| "too many predecessor records")?.checked_add(1).ok_or("too many predecessor records")?).ok_or("invalid predecessor sequence")?;
            raw.fold(&event).map_err(|error| format!("cannot reconstruct raw predecessor history: {error}"))?;
            let view = raw.day_view().map_err(|error| format!("cannot classify predecessor history: {error}"))?;
            for event in view.events {
                let id = match event.id {
                    EventId::Boot(boot) => Identity::Boot(boot),
                    EventId::Unobserved(at) => Identity::Unobserved(at),
                    EventId::Requalification(seq) => Identity::Requalification { session: original.session, night: togi_journal::NightId(seq) },
                };
                latest.insert(id.clone(), if event.class.is_cpu_suspect() { Classification::CpuSuspect { certainty: togi_journal::Certainty::Definite } } else { Classification::NonCpu {} });
                if event.class.is_cpu_suspect() {
                    charge(&mut history, id, Vec::new(), ObservedProfile::PerCore { cores: *event.profile.offsets() },
                        matches!(event.class, crate::day::Classification::ProvisionalCpuSuspect(_)), *original);
                }
            }
        }
    }
    // Merge bridge aliases after all records have been read (not just adjacent copies).
    merge_aliases(&mut history.charges);
    for charged in &mut history.charges {
        charged.reclassified = std::iter::once(&charged.event).chain(&charged.aliases)
            .any(|id| matches!(latest.get(id), Some(Classification::NonCpu {})));
        if !charged.reclassified && !history.vetoes.iter().any(|veto| veto.event == charged.event) {
            history.vetoes.push(Veto { event: charged.event.clone(), profile: charged.profile.clone(), witness: charged.witnesses[0] });
        }
    }
    history.vetoes.retain(|veto| !matches!(latest.get(&veto.event), Some(Classification::NonCpu {})));
    Ok(history)
}

fn raw_kind(kind: EventKind) -> bool {
    matches!(kind, EventKind::DayBootStart | EventKind::DayCheckpoint | EventKind::DayOffsetsChanged |
        EventKind::DayCleanStop | EventKind::DayUncleanEnd | EventKind::DayAnswer | EventKind::DayRecord |
        EventKind::DayFault | EventKind::Mce)
}

fn identity(id: &DayEventId, session: SessionId, records: &HashMap<Provenance, Identity>) -> Identity {
    match id {
        DayEventId::Boot { boot } => Identity::Boot(boot.clone()),
        DayEventId::Record { session, seq } => {
            let original = Provenance { session: *session, seq: *seq };
            records.get(&original).cloned().unwrap_or(Identity::Record(original))
        }
        DayEventId::Requalification { night } => Identity::Requalification { session, night: *night },
    }
}

fn nonzero(profile: &ObservedProfile) -> bool {
    let (ObservedProfile::Ccd { cores, .. } | ObservedProfile::PerCore { cores }) = profile;
    cores.iter().any(|(_, offset)| offset.0 != 0)
}

fn charge(history: &mut History, event: Identity, aliases: Vec<Identity>, profile: ObservedProfile, provisional: bool, witness: Provenance) {
    history.charges.push(Charge { event, aliases, profile, provisional, reclassified: false, witnesses: vec![witness] });
}

fn merge_aliases(charges: &mut Vec<Charge>) {
    let mut index = 0;
    while index < charges.len() {
        let mut seen: Vec<_> = std::iter::once(charges[index].event.clone()).chain(charges[index].aliases.iter().cloned()).collect();
        let mut other = index + 1;
        while other < charges.len() {
            if std::iter::once(&charges[other].event).chain(&charges[other].aliases).any(|id| seen.contains(id)) {
                let merged = charges.remove(other);
                if !seen.contains(&merged.event) { seen.push(merged.event); }
                for alias in merged.aliases {
                    if !seen.contains(&alias) { seen.push(alias); }
                }
                charges[index].provisional |= merged.provisional;
                charges[index].witnesses.extend(merged.witnesses);
                // A bridge can connect a previously disjoint record.
                other = index + 1;
            } else { other += 1; }
        }
        seen.retain(|id| *id != charges[index].event);
        charges[index].aliases = seen;
        charges[index].witnesses.sort_by_key(|witness| (witness.session, witness.seq));
        charges[index].witnesses.dedup();
        index += 1;
    }
}
