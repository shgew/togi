use togi_journal::{
    CcdVector, DayApplied, DayStep, DayVeto, Event, ExposureObserved, ExposureOrigin,
    NightQualification, ObservedProfile, Payload, QualificationState, Seq, StepKind, TrialId,
    VetoState,
};
use togi_machine::{BiosContext, CcdProfile};

use crate::auditor::Seen;
use crate::violation::{Check, Findings};

/// The Ruleset 13 invariants a journal states about itself (`docs/spec/journal.md`, Ruleset 13 events;
/// `docs/spec/tuner.md`, Hold and next proposal and Whole-profile veto), judged without recomputing any policy:
/// a nonzero applied day profile was night-qualified under the firmware context in force, a promotion respects
/// every active veto, and a live exposure cites the trial that produced it.
#[derive(Debug, Default)]
pub(crate) struct Rules {
    firmware: Option<BiosContext>,
    qualified: Vec<(CcdProfile, BiosContext)>,
    vetoes: Vec<DayVeto>,
}

impl Rules {
    pub(crate) fn fold(&mut self, event: &Event, seen: &Seen<'_>, findings: &mut Findings) {
        if let Payload::SessionContext(context) = &event.payload {
            self.firmware = Some(context.bios_context());
        } else if let Payload::NightQualification(qualification) = &event.payload {
            self.qualify(qualification);
        } else if let Payload::DayVeto(veto) = &event.payload {
            match self.vetoes.iter_mut().find(|held| held.event == veto.event) {
                Some(held) => *held = veto.clone(),
                None => self.vetoes.push(veto.clone()),
            }
        } else if let Payload::DayStep(step) = &event.payload {
            self.step(event.seq, step, findings);
        } else if let Payload::DayApplied(applied) = &event.payload {
            self.applied(event.seq, applied, findings);
        } else if let Payload::ExposureObserved(exposure) = &event.payload {
            exposure_cites(event, exposure, seen, findings);
        }
    }

    /// Only a complete matrix confers qualification. A qualification states its own firmware context.
    fn qualify(&mut self, qualification: &NightQualification) {
        if matches!(qualification.state, QualificationState::Complete { .. }) {
            self.qualified.push((
                qualification.profile.ccd.clone(),
                qualification.firmware.clone(),
            ));
        }
    }

    /// Every nonzero target of `day.applied` is the exact profile of an earlier complete `night.qualification`
    /// made under the firmware context the session last recorded. CO 0 needs no qualification.
    fn applied(&self, seq: Seq, applied: &DayApplied, findings: &mut Findings) {
        if applied.target.ccd.is_zero() {
            return;
        }
        let qualified = self.qualified.iter().any(|(profile, firmware)| {
            *profile == applied.target.ccd
                && self
                    .firmware
                    .as_ref()
                    .is_none_or(|current| current == firmware)
        });
        if !qualified {
            findings.add(
                seq,
                Check::Qualification,
                format!(
                    "day.applied target {} has no earlier complete night.qualification of that exact profile under the current firmware context",
                    describe(&applied.target)
                ),
            );
        }
    }

    /// A promotion's proposed profile is at or deeper than no active veto's profile.
    fn step(&self, seq: Seq, step: &DayStep, findings: &mut Findings) {
        if !matches!(step.step, StepKind::Promote { .. }) {
            return;
        }
        for veto in &self.vetoes {
            let event = &veto.event;
            if veto.state == VetoState::Active && forbids(&veto.profile, &step.proposed) {
                findings.add(
                    seq,
                    Check::Avoidance,
                    format!(
                        "day.step promote to {} while the active veto of {event:?} forbids it",
                        describe(&step.proposed)
                    ),
                );
            }
        }
    }
}

/// Whether a whole-profile veto forbids `proposed`: every CCD, or for a nonuniform profile every physical core,
/// at or deeper than the vetoed one.
fn forbids(veto: &ObservedProfile, proposed: &CcdVector) -> bool {
    match veto {
        ObservedProfile::Ccd { ccd, .. } => proposed.ccd.at_or_deeper_than(ccd),
        ObservedProfile::PerCore { cores } => {
            !cores.is_empty()
                && cores.iter().all(|(core, observed)| {
                    proposed
                        .cores
                        .get(core)
                        .is_some_and(|own| own.get() <= observed.0)
                })
        }
    }
}

fn describe(vector: &CcdVector) -> String {
    vector
        .ccd
        .iter()
        .map(|(ccd, offset)| format!("CCD{ccd}={}", offset.get()))
        .collect::<Vec<_>>()
        .join(",")
}

/// A live `exposure.observed` cites the `trial.intent`, `trial.end` or recovered `failure` that produced it, and
/// when it names a trial, one of those events belongs to that trial. An imported observation's source lies in
/// another session.
fn exposure_cites(
    event: &Event,
    exposure: &ExposureObserved,
    seen: &Seen<'_>,
    findings: &mut Findings,
) {
    if !matches!(exposure.origin, ExposureOrigin::Live {}) {
        return;
    }
    let cited: Vec<Option<&TrialId>> = event
        .cause
        .iter()
        .filter_map(|cause| seen.get(cause))
        .filter_map(|earlier| {
            if let Payload::TrialIntent(intent) = &earlier.payload {
                Some(Some(&intent.trial))
            } else if let Payload::TrialEnd(end) = &earlier.payload {
                Some(Some(&end.trial))
            } else if let Payload::Failure(failure) = &earlier.payload {
                Some(failure.trial.as_ref())
            } else {
                None
            }
        })
        .collect();
    if cited.is_empty() {
        findings.add(
            event.seq,
            Check::Exposure,
            "exposure.observed cites no existing trial.intent, trial.end or recovered failure",
        );
    } else if let Some(trial) = &exposure.source.trial
        && !cited.contains(&Some(trial))
    {
        findings.add(
            event.seq,
            Check::Exposure,
            format!("exposure.observed names trial {trial} but cites no event of that trial"),
        );
    }
}
