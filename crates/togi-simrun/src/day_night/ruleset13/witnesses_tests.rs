//! The successor witnesses fold a hand-built journal: no clock, disk or running policy.

use std::time::Duration;

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::{
    Activity, ApplicationId, ApplyCause, Binding, Build, CcdCap, CcdVector, CellEvidence,
    CellWitness, Certainty, Classification, DayApplied, DayEvent, DayEventId, DayStep, DecisionId,
    Edge, Escalation, Event, ExposureClass, ExposureObserved, ExposureOrigin, Finite, FixStamp,
    NightId, NightQualification, ObservedProfile, OriginalSource, Outcome, Payload, Placement,
    QualificationPurpose, QualificationState, Rate, ReproductionState, Resolution, RetreatCause,
    Seq, SessionContext, SessionStart, Slope, Stamp, StepCauses, StepKind, Tested, TrialEnd,
    TrialId,
};
use togi_machine::{
    BiosContext, BootId, CcdId, CcdProfile, CoreId, CoreInfo, CoreSet, CpuId, Digest,
    ObservedOffset, PerCore, Regime, TargetOffset,
};

use super::Witnesses;
use crate::day_night::successor::{
    FirmwareRestoration, Qualification, RestorationOutcome, RestorationResult,
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
            Vec::new(),
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
        step: StepKind,
        causes: StepCauses,
        hold_s: u64,
    ) {
        self.push(DayStep {
            decision: DecisionId(decision),
            cold_boot: BootId::from("boot".to_owned()),
            old: old.clone(),
            proposed: proposed.clone(),
            step,
            predecessor: None,
            hours: Vec::new(),
            hold_s,
            causes,
            reason: "test".to_owned(),
        });
        self.push(DayApplied {
            application: ApplicationId(decision),
            cause: ApplyCause::Step {
                decision: DecisionId(decision),
            },
            target: proposed.clone(),
            readbacks: readbacks(proposed),
            transactions: Vec::new(),
            boot: BootId::from("boot".to_owned()),
            mono_ns: 0,
        });
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
        StepKind::Retreat {
            changed: vec![ccd(0)],
            cause: RetreatCause::Event {
                event: DayEventId::Requalification { night },
                escalation: Escalation::EveryCcd { strike: 1 },
            },
            wanted: None,
        },
        StepCauses::default(),
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
        StepKind::FirmwareZero {
            changed: vec![ccd(0)],
        },
        StepCauses::default(),
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

    let cap = script.push(CcdCap {
        ccd: ccd(0),
        old: TargetOffset::ZERO,
        new: TargetOffset::new(-1).unwrap(),
        budget: Rate::new(0.1).unwrap(),
        edge: Edge::Unknown {},
        adequate: Vec::new(),
        uncovered: Vec::new(),
        cells: Vec::new(),
        slope: Slope::Fallback {
            beta: Finite::new(-0.1).unwrap(),
        },
        candidates: Vec::new(),
        binding: Binding::Baseline {},
    });
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
        StepKind::Promote { changed: ccd(0) },
        StepCauses {
            caps: vec![cap],
            qualification: Some(qualified),
            ..StepCauses::default()
        },
        7_200,
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
                stock_controls: Vec::new(),
                fresh_witnesses: vec!["qualifying".to_owned()],
                reused_witnesses: Vec::new(),
                safety_checks_satisfied: true,
            },
            clean_stock_hours: Duration::from_secs(7_200).as_secs_f64() / 3_600.0,
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
        StepKind::Retreat {
            changed: vec![ccd(0)],
            cause: RetreatCause::Event {
                event: boot_event(),
                escalation: Escalation::RevertNewest {
                    reverted: DecisionId(2),
                },
            },
            wanted: None,
        },
        StepCauses::default(),
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
            witnesses: vec!["ruleset13-step-3".to_owned()],
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

#[test_case(RetreatCause::ShallowerCap {}; "shallower cap")]
#[test_case(RetreatCause::Completion {}; "completion")]
fn a_non_event_retreat_to_stock_keeps_the_restoration_retained(cause: RetreatCause) {
    let mut script = restored();
    script.apply(
        3,
        &vector(1),
        &vector(0),
        StepKind::Retreat {
            changed: vec![ccd(0)],
            cause,
            wanted: None,
        },
        StepCauses::default(),
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
