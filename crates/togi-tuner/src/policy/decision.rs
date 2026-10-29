//! What the policy decides: a journal payload and the events it follows from.

use togi_journal::{
    CcdCap, CcdEdge, DayEvent, DayHold, DayInterval, DayStep, DayVeto, DeadEnd, ExposureObserved,
    Failure, NightPlan, NightQualification, NightReproduction, Payload, Seq, TunerWarning,
};

/// A commitment the policy appends to the journal.
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyPayload {
    Failure(Failure),
    ExposureObserved(ExposureObserved),
    TunerWarning(TunerWarning),
    CcdEdge(CcdEdge),
    CcdCap(CcdCap),
    UnitEdge(togi_journal::UnitEdge),
    UnitMapping(togi_journal::UnitMapping),
    ClimbDecision(togi_journal::ClimbDecision),
    UnitStop(togi_journal::UnitStop),
    CampaignStart(togi_journal::CampaignStart),
    CampaignCharge(togi_journal::CampaignCharge),
    CampaignReclassified(togi_journal::CampaignReclassified),
    FirmwareRestoration(togi_journal::FirmwareRestoration),
    SuccessorImported(togi_journal::SuccessorImported),
    NightPlan(NightPlan),
    NightQualification(NightQualification),
    NightReproduction(NightReproduction),
    DayEvent(DayEvent),
    DayVeto(DayVeto),
    DayInterval(DayInterval),
    DayHold(DayHold),
    DayStep(DayStep),
    DeadEnd(DeadEnd),
}

impl From<PolicyPayload> for Payload {
    fn from(payload: PolicyPayload) -> Self {
        match payload {
            PolicyPayload::Failure(p) => Self::Failure(p),
            PolicyPayload::ExposureObserved(p) => Self::ExposureObserved(p),
            PolicyPayload::TunerWarning(p) => Self::TunerWarning(p),
            PolicyPayload::CcdEdge(p) => Self::CcdEdge(p),
            PolicyPayload::CcdCap(p) => Self::CcdCap(p),
            PolicyPayload::UnitEdge(p) => Self::UnitEdge(p),
            PolicyPayload::UnitMapping(p) => Self::UnitMapping(p),
            PolicyPayload::ClimbDecision(p) => Self::ClimbDecision(p),
            PolicyPayload::UnitStop(p) => Self::UnitStop(p),
            PolicyPayload::CampaignStart(p) => Self::CampaignStart(p),
            PolicyPayload::CampaignCharge(p) => Self::CampaignCharge(p),
            PolicyPayload::CampaignReclassified(p) => Self::CampaignReclassified(p),
            PolicyPayload::FirmwareRestoration(p) => Self::FirmwareRestoration(p),
            PolicyPayload::SuccessorImported(p) => Self::SuccessorImported(p),
            PolicyPayload::NightPlan(p) => Self::NightPlan(p),
            PolicyPayload::NightQualification(p) => Self::NightQualification(p),
            PolicyPayload::NightReproduction(p) => Self::NightReproduction(p),
            PolicyPayload::DayEvent(p) => Self::DayEvent(p),
            PolicyPayload::DayVeto(p) => Self::DayVeto(p),
            PolicyPayload::DayInterval(p) => Self::DayInterval(p),
            PolicyPayload::DayHold(p) => Self::DayHold(p),
            PolicyPayload::DayStep(p) => Self::DayStep(p),
            PolicyPayload::DeadEnd(p) => Self::DeadEnd(p),
        }
    }
}

/// One payload to append and the journal events it cites as `cause`.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub payload: PolicyPayload,
    pub causes: Vec<Seq>,
}

impl Decision {
    pub(super) fn new(payload: PolicyPayload, causes: Vec<Seq>) -> Self {
        Self { payload, causes }
    }
}
