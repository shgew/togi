//! The successor witnesses fold a hand-built journal: no clock, disk or running policy.

use std::time::Duration;

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::{
    Activity, ApplicationId, ApplyCause, Build, CampaignCharge, CampaignEventPhase, CampaignId,
    CampaignReclassified, CcdVector, CellEvidence, CellWitness, Certainty, Classification,
    ClimbAction, ClimbDecision, ClimbHold, ClimbRetreatCause, DayApplied, DayCheckpoint, DayEvent,
    DayEventId, DayVeto, DecisionId, Event, ExposureClass, ExposureObserved, ExposureOrigin,
    FixStamp, NightId, NightQualification, ObservedProfile, OriginalSource, Outcome, Payload,
    Placement, Provenance, QualificationPurpose, QualificationState, ReproductionState, Resolution,
    RestorationKind, Seq, SessionContext, SessionStart, Stamp, Tested, TrialEnd, TrialId,
    UnitId as JournalUnit, UnitMapping, UnitMembership, UnitPhase, UnitVector, VetoState,
};
use togi_machine::{
    BiosContext, BootId, CcdId, CcdProfile, CoreId, CoreInfo, CoreSet, CpuId, Digest,
    ObservedOffset, PerCore, Regime, TargetOffset,
};

use super::Witnesses;
use crate::day_night::successor::{
    FirmwareRestoration, ProfileVeto, Qualification, RestorationOutcome, RestorationResult,
    StepKind as ReportStep, UnitId,
};

fn ccd(index: i64) -> CcdId {
    CcdId::new(index).unwrap()
}

fn core(index: i64) -> CoreId {
    CoreId::new(index).unwrap()
}

/// Two CCDs of one core each.
fn cores() -> Vec<CoreInfo> {
    (0..2)
        .map(|index| CoreInfo {
            core: core(index),
            ccd: ccd(index),
            cpus: vec![CpuId::new(index).unwrap()],
        })
        .collect()
}

/// CCD 0 at `-first`, CCD 1 at stock.
fn vector(first: i64) -> CcdVector {
    let profile = CcdProfile::zero([ccd(0), ccd(1)])
        .unwrap()
        .with(ccd(0), TargetOffset::new(-first).unwrap());
    CcdVector::expand(profile, &cores()).unwrap()
}

fn unit_vector(vector: &CcdVector) -> UnitVector {
    UnitVector {
        cores: *vector.cores(),
        mapping: UnitMapping {
            phase: UnitPhase::Coarse,
            units: (0..2)
                .map(|index| UnitMembership {
                    unit: JournalUnit::Ccd(ccd(index)),
                    cores: [core(index)].into_iter().collect(),
                    parent: None,
                })
                .collect(),
        },
    }
}

/// The report's depths for [`vector`]`(first)`.
fn depths(first: u8) -> PerCore<u8> {
    [(core(0), first), (core(1), 0)].into_iter().collect()
}

fn readbacks(vector: &CcdVector) -> PerCore<ObservedOffset> {
    vector
        .cores()
        .iter()
        .map(|(core, offset)| (core, offset.observed()))
        .collect()
}

fn context(bios: &str) -> SessionContext {
    SessionContext {
        bios_version: bios.to_owned(),
        board: "board".to_owned(),
        cpu_model: "cpu".to_owned(),
        microcode: "microcode".to_owned(),
        boost_limit_mhz: 5_000,
    }
}

fn exposure(
    trial: &str,
    activity: Activity,
    outcome: Outcome,
    profile: CcdVector,
) -> ExposureObserved {
    ExposureObserved {
        source: OriginalSource {
            session: "20231114T221320Z".parse().unwrap(),
            seq: Seq::new(1).unwrap(),
            boot: BootId::from("boot".to_owned()),
            trial: Some(TrialId::from(trial.to_owned())),
        },
        origin: ExposureOrigin::Live {},
        firmware: BiosContext::default(),
        epoch: 1,
        class: class(),
        profile,
        tested: Tested::Exact {},
        activity,
        outcome,
        failures: Vec::new(),
        eligible_s: 80,
        uninterrupted_s: 80,
        exclusions: Vec::new(),
    }
}

fn class() -> ExposureClass {
    ExposureClass {
        regime: Regime::R1,
        workloads: Vec::new(),
        cores: [core(0)].into_iter().collect::<CoreSet>(),
        position: None,
        duration_s: 90,
        placement: Placement::Exact,
    }
}

fn qualification(profile: CcdVector, state: QualificationState) -> NightQualification {
    NightQualification {
        profile,
        firmware: BiosContext::default(),
        epoch: 1,
        contents: Vec::new(),
        matrix: Digest::from_bytes([0; 32]),
        purpose: QualificationPurpose::Requalification,
        state,
    }
}

fn boot_event() -> DayEventId {
    DayEventId::Boot {
        boot: BootId::from("crash-boot".to_owned()),
    }
}

/// An in-memory journal folded by [`Witnesses`] one hand-built event at a time. Every event is one
/// hour after the one before, so each has its own day.
struct Script {
    witnesses: Witnesses,
    seq: u64,
    second: u32,
}

impl Script {
    fn new() -> Self {
        let mut script = Self {
            witnesses: Witnesses::default(),
            seq: 0,
            second: 0,
        };
        script.push(SessionStart {
            build: Build {
                version: "test".to_owned(),
                rev: None,
                ruleset: 13,
                schema: 1,
                fixes: FixStamp::none(),
                evidence: 1,
            },
            session: "20231114T221320Z".parse().unwrap(),
            cores: cores(),
        });
        script
    }

    fn push(&mut self, payload: impl Into<Payload>) -> Seq {
        self.push_caused(payload, Vec::new())
    }

    fn push_caused(&mut self, payload: impl Into<Payload>, causes: Vec<Seq>) -> Seq {
        self.seq += 1;
        self.second += 3_600;
        let seq = Seq::new(self.seq).unwrap();
        let event = Event::new(
            seq,
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: Some(Duration::from_secs(u64::from(self.second))),
            },
            BootId::from("boot".to_owned()),
            causes,
            payload.into(),
        );
        self.witnesses.observe(&event);
        seq
    }

    /// The report day of the event pushed last.
    fn day(&self) -> f64 {
        1.0 + f64::from(self.second) / 86_400.0
    }

    /// A proposal and its verified application.
    fn apply(
        &mut self,
        decision: u64,
        old: &CcdVector,
        proposed: &CcdVector,
        step: ClimbAction,
        certificate: Option<Seq>,
        hold_s: u64,
    ) -> Seq {
        let proposal = self.push(ClimbDecision {
            cold_boot: BootId::from("boot".to_owned()),
            before: unit_vector(old),
            proposed: unit_vector(proposed),
            decision: step,
            hold: ClimbHold {
                application: ApplicationId(decision),
                credited: Vec::new(),
                eligible_s: hold_s,
            },
            certificate,
            ceilings: Vec::new(),
            events: Vec::new(),
        });
        self.push(DayApplied {
            application: ApplicationId(decision),
            cause: ApplyCause::Step {
                decision: DecisionId(proposal.get()),
            },
            target: proposed.clone(),
            readbacks: readbacks(proposed),
            transactions: Vec::new(),
            boot: BootId::from("boot".to_owned()),
            mono_ns: 0,
        })
    }

    fn restorations(&self) -> Vec<FirmwareRestoration> {
        self.witnesses.applications().restorations
    }

    fn step_kinds(&self) -> Vec<ReportStep> {
        self.witnesses
            .applications()
            .steps
            .into_iter()
            .map(|step| step.kind)
            .collect()
    }
}

const FAILURE: &str = "ruleset13-failure-night-trial";

/// A failed night qualification of the exact profile cites its failed trial; the monthly
/// re-qualification event it escalates to retreats on that same trial, not a synthetic event.
#[test]
fn a_requalification_retreat_resolves_the_original_failure_identity() {
    let mut script = Script::new();
    let promoted = vector(1);
    let failure = script.push(exposure(
        "night-trial",
        Activity::Qualification,
        Outcome::Failure,
        promoted.clone(),
    ));
    let failed = script.push(qualification(
        promoted.clone(),
        QualificationState::Failed {
            failure,
            witnesses: Vec::new(),
            outstanding: Vec::new(),
        },
    ));
    let ended: TrialEnd = serde_json::from_value(serde_json::json!({
        "trial": "night-trial", "outcome": "failure", "duration_s": 90,
    }))
    .unwrap();
    script.push(ended);
    let night = NightId(Seq::new(1).unwrap());
    script.push(DayEvent {
        event: DayEventId::Requalification { night },
        profile: ObservedProfile::PerCore {
            cores: readbacks(&promoted),
        },
        boot: None,
        from: Timestamp::UNIX_EPOCH,
        to: Timestamp::UNIX_EPOCH,
        witnesses: vec![failed],
        classification: Classification::CpuSuspect {
            certainty: Certainty::Definite,
        },
        resolution: Resolution::Unresolved {},
        reason: "failed re-qualification".to_owned(),
        predecessor: None,
        escalation: None,
        cursors: Vec::new(),
        consumed: Vec::new(),
        reproduction: ReproductionState::Pending,
    });
    script.apply(
        1,
        &promoted,
        &vector(0),
        ClimbAction::Retreat {
            units: vec![JournalUnit::Ccd(ccd(0))],
            cause: ClimbRetreatCause::Event {
                event: DayEventId::Requalification { night },
            },
            wanted: None,
        },
        None,
        0,
    );

    let night_events = script.witnesses.night_events();
    assert_eq!(
        night_events
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        [FAILURE]
    );
    assert_eq!(
        script.step_kinds(),
        [ReportStep::Retreat {
            event: Some(FAILURE.to_owned()),
            units: vec![UnitId::new(0).unwrap()],
        }]
    );
}

/// A firmware change that zeroes a profile, then restores exactly that profile once qualified.
/// Returns the script with the restoration applied and retained.
fn restored() -> Script {
    let mut script = Script::new();
    script.push(context("bios-1"));
    script.push(context("bios-2"));
    script.apply(
        1,
        &vector(1),
        &vector(0),
        ClimbAction::FirmwareZero {},
        None,
        0,
    );
    let restoration = script.restorations();
    assert_eq!(
        restoration
            .iter()
            .map(|restoration| (
                &restoration.id[..],
                &restoration.previous_profile,
                &restoration.outcome
            ))
            .collect::<Vec<_>>(),
        [(
            "ruleset13-restoration-firmware-1",
            &depths(1),
            &RestorationOutcome::Blocked {
                reason: "no qualified whole-profile restoration has been applied".to_owned(),
            },
        )]
    );

    let passed = script.push(exposure(
        "qualifying",
        Activity::Qualification,
        Outcome::Pass,
        vector(1),
    ));
    let qualified = script.push(qualification(
        vector(1),
        QualificationState::Complete {
            witnesses: vec![CellWitness {
                cell: class(),
                evidence: CellEvidence::Fresh { exposure: passed },
            }],
        },
    ));
    let qualified_day = script.day();
    script.apply(
        2,
        &vector(0),
        &vector(1),
        ClimbAction::Restore {
            restoration: RestorationKind::Firmware,
            units: vec![JournalUnit::Ccd(ccd(0))],
        },
        Some(qualified),
        50 * 3_600,
    );

    let restorations = script.restorations();
    let [restoration] = restorations.as_slice() else {
        panic!("one restoration, found {restorations:?}");
    };
    assert_eq!(
        restoration.outcome,
        RestorationOutcome::Applied {
            day: script.day(),
            qualification: Qualification {
                id: format!("ruleset13-qualification-{qualified}"),
                day: qualified_day,
                epoch: "firmware-1".to_owned(),
                profile: depths(1),
                fresh_witnesses: vec!["qualifying".to_owned()],
                reused_witnesses: Vec::new(),
                safety_checks_satisfied: true,
            },
            clean_stock_hours: 50.0,
            result: RestorationResult::Retained {},
        }
    );
    assert_eq!(
        script.step_kinds(),
        [
            ReportStep::StockReset {
                epoch: "firmware-1".to_owned()
            },
            ReportStep::Restoration {
                restoration: restoration.id.clone()
            },
        ]
    );
    script
}

#[test]
fn a_retreat_event_to_stock_crashes_the_restoration_under_its_original_event() {
    let mut script = restored();
    script.apply(
        3,
        &vector(1),
        &vector(0),
        ClimbAction::Retreat {
            units: vec![JournalUnit::Ccd(ccd(0))],
            cause: ClimbRetreatCause::Event {
                event: boot_event(),
            },
            wanted: None,
        },
        None,
        0,
    );

    let event = "ruleset13-event-boot-crash-boot".to_owned();
    let restorations = script.restorations();
    let [restoration] = restorations.as_slice() else {
        panic!("one restoration, found {restorations:?}");
    };
    let RestorationOutcome::Applied { result, .. } = &restoration.outcome else {
        panic!("restoration stays applied, found {:?}", restoration.outcome);
    };
    assert_eq!(
        result,
        &RestorationResult::Crashed {
            event: event.clone(),
            stock_step_back_day: script.day(),
            stock_readback: depths(0),
            stopped_units: Vec::new(),
            witnesses: vec![format!("ruleset13-step-{}", script.seq - 1)],
        }
    );
    assert_eq!(
        script.step_kinds().last(),
        Some(&ReportStep::Retreat {
            event: Some(event),
            units: vec![UnitId::new(0).unwrap()],
        })
    );
}

#[test_case(ClimbRetreatCause::AdverseCeiling { edges: Vec::new() }; "adverse ceiling")]
#[test_case(ClimbRetreatCause::Completion {}; "completion")]
fn a_non_event_retreat_to_stock_keeps_the_restoration_retained(cause: ClimbRetreatCause) {
    let mut script = restored();
    script.apply(
        3,
        &vector(1),
        &vector(0),
        ClimbAction::Retreat {
            units: vec![JournalUnit::Ccd(ccd(0))],
            cause,
            wanted: None,
        },
        None,
        0,
    );

    let restorations = script.restorations();
    let [restoration] = restorations.as_slice() else {
        panic!("one restoration, found {restorations:?}");
    };
    let RestorationOutcome::Applied { result, .. } = &restoration.outcome else {
        panic!("restoration stays applied, found {:?}", restoration.outcome);
    };
    assert_eq!(result, &RestorationResult::Retained {});
    assert_eq!(
        script.step_kinds().last(),
        Some(&ReportStep::Retreat {
            event: None,
            units: vec![UnitId::new(0).unwrap()],
        })
    );
}

#[test_case(false, false; "complete hold and certificate")]
#[test_case(true, false; "missing hold remains visible")]
#[test_case(false, true; "missing certificate remains visible")]
fn reclassified_restoration_reports_physical_units_and_guard_witnesses(
    missing_hold: bool,
    missing_certificate: bool,
) {
    let mut script = Script::new();
    let deployed = CcdVector::expand(
        CcdProfile::uniform([ccd(0), ccd(1)], TargetOffset::new(-5).unwrap()).unwrap(),
        &cores(),
    )
    .unwrap();
    let campaign = CampaignId(Provenance {
        session: "20231114T221320Z".parse().unwrap(),
        seq: Seq::new(1).unwrap(),
    });
    script.push(CampaignCharge {
        campaign,
        event: boot_event(),
        aliases: Vec::new(),
        profile: unit_vector(&deployed),
        phase: CampaignEventPhase::Learning,
        unit_phase: UnitPhase::Coarse,
        provisional: true,
        consumed: 1,
        remaining: 1,
        exhausted: false,
    });
    let retreat = script.apply(
        1,
        &deployed,
        &vector(0),
        ClimbAction::Retreat {
            // The physical reset reverted both CCDs, irrespective of an incomplete unit list.
            units: vec![JournalUnit::Ccd(ccd(0))],
            cause: ClimbRetreatCause::Event {
                event: boot_event(),
            },
            wanted: None,
        },
        None,
        0,
    );
    let retreat_day = script.day();
    script.push(CampaignReclassified {
        campaign,
        event: boot_event(),
        from: Classification::CpuSuspect {
            certainty: Certainty::Provisional,
        },
        to: Classification::NonCpu {},
    });
    let answer_day = script.day();
    script.second += 50 * 3_600;
    let checkpoint = script.push(DayCheckpoint {
        applied_secs: 50 * 3_600,
        load_mix: togi_journal::LoadMix::default(),
        watchdog: None,
    });
    let qualified = script.push(qualification(
        deployed.clone(),
        QualificationState::Complete {
            witnesses: Vec::new(),
        },
    ));
    let decision = script.push_caused(
        ClimbDecision {
            cold_boot: BootId::from("boot".to_owned()),
            before: unit_vector(&vector(0)),
            proposed: unit_vector(&deployed),
            decision: ClimbAction::Restore {
                restoration: RestorationKind::Reclassified,
                units: vec![JournalUnit::Ccd(ccd(0)), JournalUnit::Ccd(ccd(1))],
            },
            hold: ClimbHold {
                application: ApplicationId(1),
                credited: if missing_hold {
                    Vec::new()
                } else {
                    vec![checkpoint]
                },
                eligible_s: 50 * 3_600,
            },
            certificate: (!missing_certificate).then_some(qualified),
            ceilings: Vec::new(),
            events: Vec::new(),
        },
        vec![retreat],
    );
    script.push(DayApplied {
        application: ApplicationId(2),
        cause: ApplyCause::Step {
            decision: DecisionId(decision.get()),
        },
        target: deployed.clone(),
        readbacks: readbacks(&deployed),
        transactions: Vec::new(),
        boot: BootId::from("boot".to_owned()),
        mono_ns: 0,
    });
    let applications = script.witnesses.applications();
    let [reverted, restored] = applications.steps.as_slice() else {
        panic!(
            "retreat and restoration must both be reported: {:?}",
            applications.steps
        );
    };
    let units = vec![UnitId::new(0).unwrap(), UnitId::new(1).unwrap()];
    assert_eq!(
        reverted.kind,
        ReportStep::Retreat {
            event: Some("ruleset13-event-boot-crash-boot".to_owned()),
            units: units.clone(),
        }
    );
    let ReportStep::ReclassifiedRestoration {
        retreat: source,
        event,
        reclassified_day,
        units: restored_units,
        hold,
        qualification: certificate,
        consumed_before,
        consumed_after,
        crash_vetoes,
    } = &restored.kind
    else {
        panic!(
            "restoration must not be omitted or reported as a promotion: {:?}",
            restored.kind
        );
    };
    assert_eq!(source, &reverted.id);
    assert_eq!(event, "ruleset13-event-boot-crash-boot");
    assert_eq!(*reclassified_day, answer_day);
    assert_eq!(restored_units, &units);
    assert_eq!(restored.before, depths(0));
    assert_eq!(
        restored.after,
        readbacks(&deployed)
            .iter()
            .map(|(core, value)| (core, u8::try_from(-value.0).unwrap()))
            .collect()
    );
    assert_eq!(hold.is_none(), missing_hold);
    if let Some(hold) = hold {
        assert_eq!(hold.start_day, retreat_day);
        assert_eq!(hold.profile, restored.before);
        assert_eq!(hold.clean_hours, 50.0);
        assert_eq!(hold.witnesses, [format!("ruleset13-event-{checkpoint}")]);
    }
    assert_eq!(
        certificate.as_ref().map(|item| item.id.as_str()),
        (!missing_certificate)
            .then(|| format!("ruleset13-qualification-{qualified}"))
            .as_deref()
    );
    assert_eq!((*consumed_before, *consumed_after), (1, 1));
    assert!(crash_vetoes.is_empty());
    let crashed = DayEventId::Boot {
        boot: BootId::from("restored-crash".to_owned()),
    };
    script.push(DayVeto {
        event: crashed,
        profile: ObservedProfile::PerCore {
            cores: readbacks(&deployed),
        },
        state: VetoState::Active,
        evidence: Vec::new(),
        reason: "CPU-suspect crash at the restored profile".to_owned(),
    });
    let applications = script.witnesses.applications();
    let ReportStep::ReclassifiedRestoration {
        crash_vetoes,
        consumed_before,
        consumed_after,
        ..
    } = &applications.steps.last().unwrap().kind
    else {
        unreachable!();
    };
    assert_eq!(
        crash_vetoes,
        &[ProfileVeto {
            event: "ruleset13-event-boot-restored-crash".to_owned(),
            profile: restored.after,
        }]
    );
    assert_eq!((*consumed_before, *consumed_after), (1, 1));
}
