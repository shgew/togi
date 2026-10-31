//! Journal-to-report seam fixtures, using production-run payloads rather than simulator truth.

use test_case::test_case;

use super::*;

struct Fixture {
    fold: JournalFold,
    events: Vec<Event>,
    envelope: Event,
}

impl Fixture {
    fn new(source: &[Event]) -> Self {
        let mut fixture = Self {
            fold: JournalFold::new(),
            events: Vec::new(),
            envelope: source[0].clone(),
        };
        let start = source
            .iter()
            .find(|event| matches!(event.payload, Payload::CampaignStart(_)))
            .unwrap();
        fixture.push(start.payload.clone());
        fixture
    }

    // One event per day makes the expected report times independent of the fold's day helper.
    fn push(&mut self, payload: Payload) -> Seq {
        let seq = Seq::new(u64::try_from(self.events.len()).unwrap() + 1).unwrap();
        let mut event = self.envelope.clone();
        event.seq = seq;
        event.time =
            Timestamp::from_second(i64::try_from(seq.get() - 1).unwrap() * 86_400).unwrap();
        event.cause.clear();
        event.msg = payload.message();
        event.payload = payload;
        self.fold.fold(&event).unwrap();
        self.events.push(event);
        seq
    }

    fn apply(&mut self, template: &journal::DayApplied, decision: Seq) -> Seq {
        let mut receipt = template.clone();
        receipt.application = journal::ApplicationId(u64::try_from(self.events.len()).unwrap() + 1);
        receipt.cause = journal::ApplyCause::Step {
            decision: journal::DecisionId(decision.get()),
        };
        self.push(Payload::DayApplied(receipt))
    }
}

fn day(seq: Seq) -> f64 {
    f64::from(u32::try_from(seq.get()).unwrap())
}

#[test_case(true; "backstep_before_stock_receipt")]
#[test_case(false; "backstep_after_stock_receipt")]
fn firmware_restoration_projects_saved_stock_qualified_profile_and_crash(backstep_first: bool) {
    let policy = super::tests::interrupted_run();
    let source = policy.events().unwrap();
    let mut fixture = Fixture::new(source);
    let charge = source
        .iter()
        .find_map(|event| {
            if let Payload::CampaignCharge(charge) = &event.payload {
                Some(charge.clone())
            } else {
                None
            }
        })
        .unwrap();
    let classified = source
        .iter()
        .find(|event| matches!(&event.payload, Payload::DayEvent(observed) if observed.event == charge.event))
        .unwrap();
    fixture.push(classified.payload.clone());
    fixture.push(Payload::CampaignCharge(charge));
    let allowance = fixture.fold.report.allowance.clone();
    assert_eq!(allowance.as_ref().unwrap().consumed, 1);

    let (promotion_seq, mut proposal) = source
        .iter()
        .find_map(|event| {
            if let Payload::ClimbDecision(proposal) = &event.payload
                && matches!(proposal.decision, journal::ClimbAction::Promote { .. })
            {
                Some((event.seq, proposal.clone()))
            } else {
                None
            }
        })
        .unwrap();
    let restored_receipt = source
        .iter()
        .find_map(|event| {
            if let Payload::DayApplied(receipt) = &event.payload
                && matches!(receipt.cause, journal::ApplyCause::Step { decision } if decision.0 == promotion_seq.get())
            {
                Some(receipt.clone())
            } else {
                None
            }
        })
        .unwrap();
    let stock_receipt = source
        .iter()
        .find_map(|event| {
            if let Payload::DayApplied(receipt) = &event.payload
                && receipt.readbacks.iter().all(|(_, offset)| offset.0 == 0)
            {
                Some(receipt.clone())
            } else {
                None
            }
        })
        .unwrap();
    let saved = proposal.proposed.clone();
    let previous_profile = depths(&saved.cores).unwrap();
    let stock_profile = observed_depths(&stock_receipt.readbacks).unwrap();
    assert_ne!(previous_profile, stock_profile);
    // The first promotion is deliberately nonuniform: restoring one CCD is not the whole vector.
    assert!(previous_profile.iter().any(|(_, depth)| depth == 0));
    let saved_seq = fixture.push(Payload::FirmwareRestoration(journal::FirmwareRestoration {
        saved: saved.clone(),
        state: journal::FirmwareRestorationState::Saved {},
    }));
    assert_eq!(fixture.fold.applications.restorations.len(), 0);

    proposal.before = saved.clone();
    proposal.proposed.cores = *stock_receipt.target.cores();
    proposal.decision = journal::ClimbAction::FirmwareZero {};
    proposal.certificate = None;
    let reset_decision = fixture.push(Payload::ClimbDecision(proposal.clone()));
    let reset_receipt = fixture.apply(&stock_receipt, reset_decision);
    let restoration_id = seq_id(saved_seq);
    assert_eq!(
        fixture.fold.applications.restorations,
        vec![records::FirmwareRestoration {
            id: restoration_id.clone(),
            epoch: "firmware-1".to_owned(),
            transition_day: day(saved_seq),
            previous_profile,
            membership: membership(&saved.mapping).unwrap(),
            stock_reset_day: day(reset_receipt),
            stock_readback: stock_profile,
            outcome: records::RestorationOutcome::Blocked {
                reason: "awaiting journaled qualification, stock hold and application".to_owned(),
            },
        }]
    );
    assert_eq!(
        fixture.fold.applications.steps.last().unwrap(),
        &records::ProfileStep {
            id: seq_id(reset_decision),
            day: day(reset_receipt),
            before: previous_profile,
            after: stock_profile,
            kind: records::StepKind::StockReset {
                epoch: "firmware-1".to_owned()
            },
        }
    );

    let mut certificate = source
        .iter()
        .find_map(|event| {
            if let Payload::NightQualification(certificate) = &event.payload
                && matches!(
                    certificate.state,
                    journal::QualificationState::Complete { .. }
                )
            {
                Some(certificate.clone())
            } else {
                None
            }
        })
        .unwrap();
    certificate.profile = restored_receipt.target.clone();
    certificate.purpose = journal::QualificationPurpose::Firmware;
    let mut exposure = source
        .iter()
        .find_map(|event| {
            if let Payload::ExposureObserved(exposure) = &event.payload {
                Some(exposure.clone())
            } else {
                None
            }
        })
        .unwrap();
    exposure.firmware = certificate.firmware.clone();
    exposure.epoch = certificate.epoch;
    exposure.profile = restored_receipt.target.clone();
    exposure.activity = journal::Activity::Qualification;
    exposure.outcome = journal::Outcome::Pass;
    exposure.failures.clear();
    exposure.exclusions.clear();
    exposure.source.trial = Some(journal::TrialId::from(
        "restoration-qualification".to_owned(),
    ));
    let fresh_trial = exposure.source.trial.as_ref().unwrap().as_str().to_owned();
    let fresh_exposure = fixture.push(Payload::ExposureObserved(exposure.clone()));
    certificate.state = journal::QualificationState::Complete {
        witnesses: vec![journal::CellWitness {
            cell: exposure.class,
            evidence: journal::CellEvidence::Fresh {
                exposure: fresh_exposure,
            },
        }],
    };
    let certificate_seq = fixture.push(Payload::NightQualification(certificate));
    proposal.before = proposal.proposed.clone();
    proposal.proposed = saved.clone();
    let units: Vec<_> = saved.mapping.units.iter().map(|unit| unit.unit).collect();
    proposal.decision = journal::ClimbAction::Restore {
        restoration: journal::RestorationKind::Firmware,
        units: units.clone(),
    };
    proposal.certificate = Some(certificate_seq);
    proposal.hold.application = journal::ApplicationId(reset_receipt.get());
    proposal.hold.eligible_s = 72 * 3600;
    proposal.hold.credited = vec![reset_receipt];
    let restore_decision = fixture.push(Payload::ClimbDecision(proposal));
    let restore_receipt = fixture.apply(&restored_receipt, restore_decision);
    let qualification = records::Qualification {
        id: seq_id(certificate_seq),
        day: day(certificate_seq),
        epoch: "firmware-1".to_owned(),
        profile: previous_profile,
        fresh_witnesses: vec![fresh_trial],
        reused_witnesses: Vec::new(),
        safety_checks_satisfied: true,
    };
    assert_eq!(
        fixture.fold.applications.restorations[0].outcome,
        records::RestorationOutcome::Applied {
            day: day(restore_receipt),
            qualification: qualification.clone(),
            clean_stock_hours: 72.0,
            result: records::RestorationResult::Retained {},
        }
    );
    assert_eq!(fixture.fold.report.readback, Some(previous_profile));
    assert_eq!(
        fixture.fold.report.newest_step,
        Some(records::StepIdentity::Restoration {
            restoration: restoration_id.clone(),
            units: units
                .iter()
                .copied()
                .map(|unit| unit_id(unit).unwrap())
                .collect(),
        })
    );
    assert_eq!(
        fixture.fold.applications.steps.last().unwrap(),
        &records::ProfileStep {
            id: seq_id(restore_decision),
            day: day(restore_receipt),
            before: stock_profile,
            after: previous_profile,
            kind: records::StepKind::Restoration {
                restoration: restoration_id
            },
        }
    );
    assert_eq!(fixture.fold.report.allowance, allowance);

    let crash = journal::DayEventId::Boot {
        boot: restored_receipt.boot,
    };
    let mut stops = Vec::new();
    for unit in &units {
        let witness = fixture.push(Payload::UnitStop(journal::UnitStop {
            unit: *unit,
            reason: journal::UnitStopReason::RestorationBackstep {
                event: crash.clone(),
            },
            profile: saved.clone(),
            last_clean: TargetOffset::ZERO,
            witnesses: vec![restore_receipt],
        }));
        stops.push(records::UnitFinish {
            unit: unit_id(*unit).unwrap(),
            reason: records::FinishReason::FirmwareRestorationStop,
            day: day(witness),
            witnesses: vec![seq_id(restore_receipt), event_id(&crash)],
        });
    }
    let backstep = Payload::FirmwareRestoration(journal::FirmwareRestoration {
        saved,
        state: journal::FirmwareRestorationState::SteppedBack {
            event: crash.clone(),
        },
    });
    let mut rollback = stock_receipt;
    rollback.cause = journal::ApplyCause::Reset {
        cause: restore_receipt,
    };
    let completion = if backstep_first {
        fixture.push(backstep);
        assert_eq!(fixture.fold.report.readback, Some(previous_profile));
        fixture.push(Payload::DayApplied(rollback))
    } else {
        fixture.push(Payload::DayApplied(rollback));
        fixture.push(backstep)
    };
    let stopped_units: Vec<_> = units
        .iter()
        .copied()
        .map(|unit| unit_id(unit).unwrap())
        .collect();
    assert_eq!(fixture.fold.report.finishes, stops);
    assert_eq!(fixture.fold.report.stopped_units, stopped_units);
    assert_eq!(fixture.fold.report.readback, Some(stock_profile));
    assert_eq!(fixture.fold.report.allowance, allowance);
    assert_eq!(
        fixture.fold.applications.restorations[0].outcome,
        records::RestorationOutcome::Applied {
            day: day(restore_receipt),
            qualification,
            clean_stock_hours: 72.0,
            result: records::RestorationResult::Crashed {
                event: event_id(&crash),
                stock_step_back_day: day(completion),
                stock_readback: stock_profile,
                stopped_units,
                witnesses: vec![seq_id(completion)],
            },
        }
    );
}

#[test_case(false; "qualification_links_trial_end")]
#[test_case(true; "qualification_links_exposure")]
fn failed_night_has_one_identity_and_only_requalification_is_charged(from_exposure: bool) {
    let policy = super::tests::interrupted_run();
    let source = policy.events().unwrap();
    let mut fixture = Fixture::new(source);
    let mut end = source
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end.clone())
            } else {
                None
            }
        })
        .unwrap();
    let mut certificate = source
        .iter()
        .find_map(|event| {
            if let Payload::NightQualification(certificate) = &event.payload {
                Some(certificate.clone())
            } else {
                None
            }
        })
        .unwrap();
    let mut exposure = source
        .iter()
        .find_map(|event| {
            if let Payload::ExposureObserved(exposure) = &event.payload {
                Some(exposure.clone())
            } else {
                None
            }
        })
        .unwrap();
    end.outcome = journal::Outcome::Failure;
    let candidate_trial = end.trial.clone();
    let candidate_failure = fixture.push(Payload::TrialEnd(end.clone()));
    certificate.purpose = journal::QualificationPurpose::NextStep;
    certificate.state = journal::QualificationState::Failed {
        failure: candidate_failure,
        witnesses: Vec::new(),
        outstanding: Vec::new(),
    };
    fixture.push(Payload::NightQualification(certificate.clone()));
    let candidate_id = format!("journal-failure-{}", candidate_trial.as_str());
    assert_eq!(fixture.fold.report.events.len(), 1);
    assert_eq!(fixture.fold.report.events[0].id, candidate_id);
    assert!(!fixture.fold.report.events[0].charged);
    assert_eq!(fixture.fold.report.allowance.as_ref().unwrap().consumed, 0);

    end.trial = source
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(other) = &event.payload
                && other.trial != candidate_trial
            {
                Some(other.trial.clone())
            } else {
                None
            }
        })
        .unwrap();
    let requalification_id = format!("journal-failure-{}", end.trial.as_str());
    let failure = fixture.push(Payload::TrialEnd(end.clone()));
    exposure.source.trial = Some(end.trial);
    exposure.outcome = journal::Outcome::Failure;
    let exposure_seq = fixture.push(Payload::ExposureObserved(exposure));
    certificate.purpose = journal::QualificationPurpose::Requalification;
    certificate.state = journal::QualificationState::Failed {
        failure: if from_exposure { exposure_seq } else { failure },
        witnesses: Vec::new(),
        outstanding: Vec::new(),
    };
    let failed_certificate = fixture.push(Payload::NightQualification(certificate));
    let mut observed = source
        .iter()
        .find_map(|event| {
            if let Payload::DayEvent(observed) = &event.payload {
                Some(observed.clone())
            } else {
                None
            }
        })
        .unwrap();
    observed.event = journal::DayEventId::Requalification {
        night: journal::NightId(failure),
    };
    observed.witnesses = vec![failed_certificate];
    observed.classification = journal::Classification::CpuSuspect {
        certainty: journal::Certainty::Provisional,
    };
    fixture.push(Payload::DayEvent(observed.clone()));
    let mut charge = source
        .iter()
        .find_map(|event| {
            if let Payload::CampaignCharge(charge) = &event.payload {
                Some(charge.clone())
            } else {
                None
            }
        })
        .unwrap();
    charge.event = observed.event.clone();
    charge.aliases.clear();
    charge.provisional = true;
    charge.consumed = 1;
    charge.remaining = 1;
    charge.exhausted = false;
    charge.phase = journal::CampaignEventPhase::Learning;
    fixture.push(Payload::CampaignCharge(charge));
    let failed_trials: Vec<_> = fixture
        .events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload
                && end.outcome == journal::Outcome::Failure
            {
                Some(end)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(failed_trials.len(), 2);
    for end in failed_trials {
        let id = format!("journal-failure-{}", end.trial.as_str());
        assert_eq!(
            fixture
                .fold
                .report
                .events
                .iter()
                .filter(|event| event.id == id)
                .count(),
            1,
            "trial {}",
            end.trial
        );
    }
    assert_eq!(fixture.fold.report.events.len(), 2);
    assert!(!fixture.fold.report.events[0].charged);
    let reported = &fixture.fold.report.events[1];
    assert_eq!(reported.id, requalification_id);
    assert_eq!(reported.day, day(failure));
    assert_eq!(reported.classification, records::Classification::CpuSuspect);
    assert_eq!((reported.provisional, reported.charged), (true, true));
    assert_eq!(reported.phase, records::Phase::Learning);
    let ledger = fixture.fold.report.allowance.as_ref().unwrap();
    assert_eq!(
        (ledger.limit, ledger.consumed, ledger.provisional),
        (2, 1, 1)
    );

    // A revised DayEvent updates that same trial identity, not a new report event.
    observed.classification = journal::Classification::NonCpu {};
    fixture.push(Payload::DayEvent(observed));
    assert_eq!(fixture.fold.report.events.len(), 2);
    let revised = &fixture.fold.report.events[1];
    assert_eq!(revised.id, requalification_id);
    assert_eq!(revised.classification, records::Classification::NonCpu);
    assert_eq!(
        revised.reclassified_from,
        Some(records::Classification::CpuSuspect)
    );
    assert_eq!((revised.provisional, revised.charged), (false, true));
}
