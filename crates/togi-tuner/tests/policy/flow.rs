//! Day and night through the journal: a qualified proposal promoted and applied, the firmware zero and the
//! retained profile's re-qualification, an unexplained end's provisional veto and retreat, its one-night
//! reproduction, a reclassified step back's restoration, and a stock failure's dead end.

use std::collections::BTreeMap;

use togi_journal::{
    Activity, ApplicationId, ApplyCause, CcdVector, CellEvidence, Classification, DayApplied,
    DayCheckpoint, DayCleanStop, DayOffsetsChanged, DayStep, DayUncleanEnd, Edge, ExposureClass,
    LoadMix, NightQualification, Payload, QualificationPurpose, QualificationState, Queue,
    ReproductionState, RetreatCause, Seq, StepKind, Tested, UncoveredReason, WarningKind,
};
use togi_machine::{BootId, CcdProfile, EndEvidence, Regime, ResetReason};
use togi_tuner::cap::Rate;
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::night::Roles;
use togi_tuner::policy::{Action, DayQuery, EndReason, Params, PolicyPayload, TrialPlan};

use crate::fixture::{Ending, Journal, at, ccd, firmware, offsets_of, rebuilt, zero};

pub(crate) fn cold_boot(boot: &str, second: i64) -> DayQuery {
    DayQuery {
        boot: BootId::from(boot.to_owned()),
        invocation: Invocation::ColdBoot {
            pause: Pause::Running,
        },
        now: at(second),
    }
}

fn cold_boot_at(boot: &str, now: jiff::Timestamp) -> DayQuery {
    DayQuery {
        boot: BootId::from(boot.to_owned()),
        invocation: Invocation::ColdBoot {
            pause: Pause::Running,
        },
        now,
    }
}

/// The steps among `payloads`.
pub(crate) fn steps(payloads: &[Payload]) -> Vec<&DayStep> {
    payloads
        .iter()
        .filter_map(|payload| {
            if let Payload::DayStep(step) = payload {
                Some(step)
            } else {
                None
            }
        })
        .collect()
}

/// Applies `step` in `boot` as the session would after its readback verified, returning the application's seq.
pub(crate) fn apply(journal: &mut Journal, step: &DayStep, application: u64, boot: &str) -> Seq {
    let cause = journal
        .events
        .iter()
        .rev()
        .find(|event| matches!(&event.payload, Payload::DayStep(committed) if committed == step))
        .map(|event| event.seq)
        .unwrap();
    journal.append(
        DayApplied {
            application: ApplicationId(application),
            cause: ApplyCause::Step {
                decision: step.decision,
            },
            target: step.proposed.clone(),
            readbacks: offsets_of(step.proposed.ccd()),
            transactions: Vec::new(),
            boot: BootId::from(boot.to_owned()),
            mono_ns: 1,
        }
        .into(),
        &[cause],
    )
}

/// The qualifications committed so far.
pub(crate) fn qualifications(journal: &Journal) -> Vec<&NightQualification> {
    journal
        .events
        .iter()
        .filter_map(|event| {
            if let Payload::NightQualification(record) = &event.payload {
                Some(record)
            } else {
                None
            }
        })
        .collect()
}

/// Runs the first night until the filler, closes it at the cold boot `day1`, settles the day and applies
/// its promotion: D is then the qualified proposal.
pub(crate) fn promoted() -> (Journal, CcdProfile) {
    promoted_with(Params::SPEC)
}

/// [`promoted`] under a policy with `params`.
fn promoted_with(params: Params) -> (Journal, CcdProfile) {
    let mut journal = Journal::ready_with(params);
    journal.until_filler(|_| Ending::Pass);
    let qualified = qualifications(&journal)
        .into_iter()
        .rev()
        .find(|record| {
            record.purpose == QualificationPurpose::NextStep
                && matches!(record.state, QualificationState::Complete { .. })
        })
        .map(|record| record.profile.ccd().clone())
        .unwrap_or_else(|| panic!("the night qualified no proposal"));
    journal.boot_start("day1");
    let committed = journal.settle_day(&cold_boot("day1", 60_000));
    let step = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::Promote { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no promotion in {committed:?}"));
    assert_eq!(*step.proposed.ccd(), qualified);
    apply(&mut journal, &step, 1, "day1");
    (journal, qualified)
}

#[test]
fn a_qualified_proposal_is_promoted_at_the_cold_boot_and_its_application_commits_d() {
    let (journal, qualified) = promoted();
    assert!(!qualified.is_zero());
    assert_eq!(
        journal.state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&qualified))
    );
}

#[test]
fn a_firmware_change_zeroes_a_nonzero_d_and_the_next_night_requalifies_it_after_the_controls() {
    let (mut journal, d) = promoted();
    // The firmware update clears the offsets; the next cold boot reports the new firmware.
    journal.boot_start_at("day2", "2", &zero());
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let step = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::FirmwareZero { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no firmware zero in {committed:?}"));
    assert!(step.proposed.ccd().is_zero());
    apply(&mut journal, &step, 2, "day2");
    assert_eq!(
        journal.state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&zero()))
    );
    // The tuning night runs the stock controls first, then re-qualifies the retained D exactly.
    journal.reboot("tuning2");
    journal.append(firmware("2").into(), &[]);
    for step in 0..3_000_i64 {
        let plan = journal.until_trial(at(62_000 + step));
        if step == 0 {
            assert_eq!(plan.activity, Activity::Control, "{plan:?}");
        }
        if qualifications(&journal).iter().any(|record| {
            record.purpose == QualificationPurpose::Firmware && *record.profile.ccd() == d
        }) {
            assert_eq!(plan.tested, Tested::Exact {});
            return;
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("D was never re-qualified under the new firmware");
}

/// Ends the boot `day1` uncleanly at D, with nothing explaining the end, and starts the cold boot `day2`.
fn unexplained_end(journal: &mut Journal, d: &CcdProfile) {
    journal.boot_start_at("day2", "1", &zero());
    journal.append(
        DayUncleanEnd {
            boot: BootId::from("day1".to_owned()),
            last_checkpoint: None,
            offsets: offsets_of(d),
            reset_reason: ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: EndEvidence::unknown(),
        }
        .into(),
        &[],
    );
}

#[test]
fn an_unexplained_end_at_d_vetoes_and_retreats_provisionally_until_an_answer_reclassifies_it() {
    let (mut journal, d) = promoted();
    unexplained_end(&mut journal, &d);
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let event = committed
        .iter()
        .find_map(|payload| {
            if let Payload::DayEvent(event) = payload {
                Some(event)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no event in {committed:?}"));
    assert!(
        matches!(event.classification, Classification::CpuSuspect { .. }),
        "{event:?}"
    );
    assert!(
        committed
            .iter()
            .any(|payload| matches!(payload, Payload::DayVeto(_)))
    );
    let retreat = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::Retreat { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no retreat in {committed:?}"));
    let StepKind::Retreat { cause, .. } = &retreat.step else {
        unreachable!();
    };
    assert!(matches!(cause, RetreatCause::Event { .. }), "{cause:?}");
    assert!(retreat.proposed.ccd().is_zero());
    // The user answers that they turned the machine off: the end is no CPU evidence, and the veto lifts.
    journal.append(
        togi_journal::DayAnswer {
            boot: BootId::from("day1".to_owned()),
            offsets: offsets_of(&d),
            answer: togi_journal::DayAnswerKind::PowerOff,
            uid: 1000,
        }
        .into(),
        &[],
    );
    let answered = journal.settle_day(&cold_boot("day2", 61_100));
    let reclassified = answered
        .iter()
        .find_map(|payload| {
            if let Payload::DayEvent(event) = payload {
                Some(event)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no reclassified event in {answered:?}"));
    assert!(
        !matches!(
            reclassified.classification,
            Classification::CpuSuspect { .. }
        ),
        "{reclassified:?}"
    );
}

#[test]
fn a_reproduction_gets_one_night_even_across_a_crash_and_is_unreproduced_when_it_closes() {
    let (mut journal, d) = promoted();
    unexplained_end(&mut journal, &d);
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let retreat = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::Retreat { .. }))
        .cloned()
        .unwrap();
    apply(&mut journal, &retreat, 2, "day2");
    // The night reproduces the failed profile exactly first.
    journal.reboot("tuning2");
    let plan = journal.until_trial(at(62_000));
    assert_eq!(plan.task.queue, Queue::Reproduction);
    assert_eq!(plan.tested, Tested::Exact {});
    assert_eq!(plan.profile.to_ccd().unwrap(), d);
    let night = plan.night;
    journal.run(&plan, Ending::Pass);
    // A crash reboot back into tuning continues the same night and the same reproduction.
    journal.reboot("tuning2-after-crash");
    let plan = journal.until_trial(at(62_100));
    assert_eq!(plan.night, night);
    assert_eq!(plan.task.queue, Queue::Reproduction);
    journal.run(&plan, Ending::Pass);
    // The next cold boot closes the night: its first commitment marks the clean attempt unreproduced.
    journal.boot_start("day3");
    let closure = journal.settle_day(&cold_boot("day3", 63_000));
    let reproduction = closure
        .iter()
        .find_map(|payload| {
            if let Payload::NightReproduction(record) = payload {
                Some(record)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no reproduction closure in {closure:?}"));
    assert_eq!(reproduction.night, night);
    assert_eq!(reproduction.state, ReproductionState::Unreproduced);
    // The next night never returns to that event.
    journal.reboot("tuning3");
    for step in 0..30_i64 {
        let plan = journal.until_trial(at(64_000 + step));
        assert_ne!(plan.night, night);
        assert_ne!(plan.task.queue, Queue::Reproduction, "{plan:?}");
        journal.run(&plan, Ending::Pass);
    }
}

#[test]
fn a_conclusive_failure_of_the_stock_controls_is_a_dead_end() {
    let mut journal = Journal::ready();
    let plan = journal.until_trial(at(100));
    assert_eq!(plan.activity, Activity::Control);
    assert!(plan.profile.is_zero());
    journal.run(&plan, Ending::Fail(Some(0)));
    assert_eq!(
        journal.until_end(at(200), &BTreeMap::new()).unwrap_err(),
        EndReason::DeadEnd
    );
    assert!(
        journal
            .events
            .iter()
            .any(|event| matches!(event.payload, Payload::DeadEnd(_)))
    );
    assert!(journal.state.project().stopped);
    // A restart asks nothing more.
    assert_eq!(
        journal.until_end(at(300), &BTreeMap::new()).unwrap_err(),
        EndReason::DeadEnd
    );
}

#[test]
fn a_stock_failure_whose_commitments_are_still_owed_already_establishes_the_dead_end() {
    let mut journal = Journal::ready();
    let plan = journal.until_trial(at(100));
    assert_eq!(journal.stock_dead_end(at(150)).unwrap(), None);
    journal.run(&plan, Ending::Fail(Some(0)));
    // The failure and its exposure are still owed: the night would commit them before its dead end.
    let Ok(Action::Decide(owed)) = journal.next(at(200)) else {
        panic!("expected an owed commitment");
    };
    assert!(
        !matches!(owed.payload, PolicyPayload::DeadEnd(_)),
        "{owed:?}"
    );
    let events = journal.events.len();
    let dead_end = journal
        .stock_dead_end(at(200))
        .unwrap()
        .unwrap_or_else(|| panic!("no dead end before the owed commitments"));
    assert_eq!(journal.events.len(), events);
    assert_eq!(dead_end.causes, vec![journal.events.last().unwrap().seq]);
    // It is the dead end the drained night commits.
    assert_eq!(
        journal.until_end(at(300), &BTreeMap::new()).unwrap_err(),
        EndReason::DeadEnd
    );
    let committed = journal
        .events
        .iter()
        .find_map(|event| {
            if let Payload::DeadEnd(committed) = &event.payload {
                Some(committed)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(PolicyPayload::DeadEnd(committed.clone()), dead_end.payload);
    // Once recorded, nothing more is established.
    assert_eq!(journal.stock_dead_end(at(400)).unwrap(), None);
}

#[test]
fn a_monthly_requalification_of_d_runs_every_cell_afresh() {
    let (mut journal, d) = promoted();
    journal.advance(31 * 24 * 3_600);
    night_until_filler(&mut journal, "tuning2");
    let requalified = qualifications(&journal)
        .into_iter()
        .rev()
        .find(|record| {
            record.purpose == QualificationPurpose::Requalification
                && *record.profile.ccd() == d
                && matches!(record.state, QualificationState::Complete { .. })
        })
        .unwrap_or_else(|| panic!("D was never re-qualified"));
    let QualificationState::Complete { witnesses } = &requalified.state else {
        unreachable!();
    };
    assert!(!witnesses.is_empty());
    assert!(
        witnesses
            .iter()
            .all(|witness| matches!(witness.evidence, CellEvidence::Fresh { .. })),
        "{witnesses:?}"
    );
}

/// The projection after an unexplained end's provisional retreat, pinned except for the cap's class lists
/// and the night's task detail: the units, the day, its event and veto, and the night that closed. It starts
/// from the compact promoted day, so every sequence it pins is one of that scenario's own events.
#[test]
fn the_projection_after_a_provisional_retreat_matches_its_snapshot() {
    let (mut journal, d) = Journal::promoted_compact();
    unexplained_end(&mut journal, &d);
    journal.settle_day(&cold_boot("day2", 61_000));
    let projection = serde_json::to_value(journal.state.project()).unwrap();
    let pinned = serde_json::json!({
        "applied": projection["applied"],
        "units": projection["units"]
            .as_array()
            .unwrap()
            .iter()
            .map(|unit| serde_json::json!({
                "unit": unit["unit"],
                "cores": unit["cores"],
                "edge": unit["edge"],
                "cap": unit["cap"]["offset"],
                "binding": unit["cap"]["binding"]["kind"],
                "day": unit["day"],
                "remaining": unit["remaining"],
            }))
            .collect::<Vec<_>>(),
        "day": projection["day"],
        "events": projection["events"],
        "vetoes": projection["vetoes"],
        "night": {
            "night": projection["night"]["night"],
            "closed_by": projection["night"]["closed_by"],
            "budgets": projection["night"]["budgets"],
            "unavailable": projection["night"]["unavailable"],
            "filler": projection["night"]["filler"],
        },
        "stopped": projection["stopped"],
    });
    if std::env::var_os("TOGI_PRINT_SNAPSHOT").is_some() {
        println!("{}", serde_json::to_string_pretty(&pinned).unwrap());
    }
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("snapshots/provisional_projection.json")).unwrap();
    assert_eq!(pinned, golden);
}

/// Ends the boot `ended` uncleanly at `at_profile`, with nothing explaining the end, and starts the cold
/// boot `next`.
fn unexplained_end_of(journal: &mut Journal, ended: &str, next: &str, at_profile: &CcdProfile) {
    journal.boot_start_at(next, "1", &zero());
    journal.append(
        DayUncleanEnd {
            boot: BootId::from(ended.to_owned()),
            last_checkpoint: None,
            offsets: offsets_of(at_profile),
            reset_reason: ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: EndEvidence::unknown(),
        }
        .into(),
        &[],
    );
}

/// Runs a night in `boot` until it asks for its filler, each trial passing.
fn night_until_filler(journal: &mut Journal, boot: &str) -> Vec<TrialPlan> {
    journal.reboot(boot);
    let mut plans = Vec::new();
    for _ in 0..3_500 {
        let plan = journal.until_trial(journal.now());
        if plan.task.queue == Queue::Filler {
            return plans;
        }
        journal.run(&plan, Ending::Pass);
        plans.push(plan);
    }
    panic!("the night never reached its filler");
}

/// The committed cap's offset on `ccd`.
fn cap_of(journal: &Journal, ccd: usize) -> i64 {
    serde_json::to_value(journal.state.project()).unwrap()["units"][ccd]["cap"]["offset"]
        .as_i64()
        .unwrap()
}

#[test]
fn a_fallback_to_co_zero_qualifies_the_wanted_profile_first_and_the_day_restores_it() {
    // A budget generous enough for caps several steps deep.
    let params = Params {
        machine_budget: Rate::per_hour(1.0).unwrap(),
        ..Params::SPEC
    };
    let (mut journal, first) = promoted_with(params);
    // The first D runs a full hold, then a night qualifies the next proposal and a cold boot promotes it.
    journal.append(
        DayOffsetsChanged {
            offsets: offsets_of(&first),
        }
        .into(),
        &[],
    );
    journal.advance(51 * 3_600);
    journal.append(
        DayCheckpoint {
            applied_secs: 51 * 3_600,
            load_mix: LoadMix::default(),
            watchdog: None,
        }
        .into(),
        &[],
    );
    journal.append(DayCleanStop {}.into(), &[]);
    night_until_filler(&mut journal, "tuning1");
    journal.boot_start_at("day2", "1", &first);
    let committed = journal.settle_day(&cold_boot_at("day2", journal.now()));
    let second = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::Promote { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no second promotion in {committed:?}"));
    apply(&mut journal, &second, 2, "day2");
    // A month later the first D's certificate has lapsed, and deep mapping failures on CCD 0 pull its
    // cap in: the ordinary proposal from CO 0 is no longer the first D.
    journal.advance(31 * 24 * 3_600);
    journal.reboot("tuning2");
    let deep = cap_of(&journal, 0);
    while cap_of(&journal, 0) == deep {
        let plan = journal.until_trial(journal.now());
        let fails = plan.activity == Activity::Mapping
            && plan
                .profile
                .get(togi_tuner::UnitId::Ccd(ccd(0)))
                .is_some_and(|offset| offset.get() <= -19);
        journal.run(
            &plan,
            if fails {
                Ending::Fail(None)
            } else {
                Ending::Pass
            },
        );
    }
    let pulled = cap_of(&journal, 0);
    let wanted_ccd0 = first.get(ccd(0)).unwrap().get();
    assert!((deep..wanted_ccd0).contains(&pulled), "{pulled}");
    // An unexplained end at the second D steps back to the first, which unqualified is held at CO 0.
    unexplained_end_of(&mut journal, "day2", "day3", second.proposed.ccd());
    let committed = journal.settle_day(&cold_boot_at("day3", journal.now()));
    let fallback = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::Retreat { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no retreat in {committed:?}"));
    let StepKind::Retreat { wanted, .. } = &fallback.step else {
        unreachable!();
    };
    assert_eq!(wanted.as_ref().map(CcdVector::ccd), Some(&first));
    assert!(fallback.proposed.ccd().is_zero());
    apply(&mut journal, &fallback, 3, "day3");
    // The next night qualifies the wanted profile, not the ordinary proposal from CO 0.
    let plans = night_until_filler(&mut journal, "tuning3");
    let qualified: Vec<CcdProfile> = plans
        .iter()
        .filter(|plan| plan.activity == Activity::Qualification && !plan.profile.is_zero())
        .map(|plan| plan.profile.to_ccd().unwrap())
        .collect();
    assert_eq!(qualified.first(), Some(&first), "{qualified:?}");
    // The next cold boot restores it: a retreat completing the event, not a promotion.
    journal.boot_start("day4");
    let committed = journal.settle_day(&cold_boot_at("day4", journal.now()));
    let restored = steps(&committed)
        .into_iter()
        .find(|step| *step.proposed.ccd() == first)
        .unwrap_or_else(|| panic!("the wanted profile was never restored: {committed:?}"));
    assert!(
        matches!(
            restored.step,
            StepKind::Retreat {
                cause: RetreatCause::Completion { .. },
                ..
            }
        ),
        "{restored:?}"
    );
}

/// Runs `d` for a full hold in the current boot, then stops the boot cleanly.
fn held_at(journal: &mut Journal, d: &CcdProfile) {
    journal.append(
        DayOffsetsChanged {
            offsets: offsets_of(d),
        }
        .into(),
        &[],
    );
    journal.advance(51 * 3_600);
    journal.append(
        DayCheckpoint {
            applied_secs: 51 * 3_600,
            load_mix: LoadMix::default(),
            watchdog: None,
        }
        .into(),
        &[],
    );
    journal.append(DayCleanStop {}.into(), &[]);
}

/// The only step of `payloads` that `kind` matches.
fn step_of(payloads: &[Payload], kind: impl Fn(&StepKind) -> bool) -> DayStep {
    steps(payloads)
        .into_iter()
        .find(|step| kind(&step.step))
        .cloned()
        .unwrap_or_else(|| panic!("no such step in {payloads:?}"))
}

#[test]
fn a_reclassified_step_back_commits_a_restoration_citing_its_answer_event_promotion_and_retreat() {
    let (mut journal, first) = promoted_with(deep());
    held_at(&mut journal, &first);
    night_until_filler(&mut journal, "tuning1");
    journal.boot_start_at("day2", "1", &first);
    let committed = journal.settle_day(&cold_boot_at("day2", journal.now()));
    let promotion = step_of(&committed, |step| matches!(step, StepKind::Promote { .. }));
    apply(&mut journal, &promotion, 2, "day2");
    let promoted_at = journal.events.last().unwrap().seq;

    // An unexplained end at the second D steps back to the qualified, nonzero first D.
    unexplained_end_of(&mut journal, "day2", "day3", promotion.proposed.ccd());
    let committed = journal.settle_day(&cold_boot_at("day3", journal.now()));
    let retreat = step_of(&committed, |step| matches!(step, StepKind::Retreat { .. }));
    assert_eq!(*retreat.proposed.ccd(), first, "{retreat:?}");
    apply(&mut journal, &retreat, 3, "day3");
    let retreated_at = journal.events.last().unwrap().seq;
    held_at(&mut journal, &first);

    // A graphics answer reclassifies the end: the next cold boot owes the restoration.
    let answered_at = journal.append(
        togi_journal::DayAnswer {
            boot: BootId::from("day2".to_owned()),
            offsets: offsets_of(promotion.proposed.ccd()),
            answer: togi_journal::DayAnswerKind::Graphics,
            uid: 1000,
        }
        .into(),
        &[],
    );
    journal.boot_start_at("day4", "1", &first);
    let committed = journal.settle_day(&cold_boot_at("day4", journal.now()));
    let restoration = step_of(&committed, |step| matches!(step, StepKind::Promote { .. }));
    assert_eq!(restoration.old, retreat.proposed);
    assert_eq!(restoration.proposed, promotion.proposed);
    assert_eq!(restoration.step, promotion.step, "the same CCD changes");
    assert!(
        restoration.reason.starts_with("restore CCD"),
        "{restoration:?}"
    );
    let event = journal
        .events
        .iter()
        .rev()
        .find(|event| matches!(&event.payload, Payload::DayEvent(_)))
        .map(|event| event.seq)
        .unwrap();
    assert_eq!(restoration.causes.events, [event]);
    let cited = &journal
        .events
        .iter()
        .rev()
        .find(|committed| {
            matches!(&committed.payload, Payload::DayStep(step) if *step == restoration)
        })
        .unwrap()
        .cause;
    for (witness, seq) in [
        ("answer", answered_at),
        ("event", event),
        ("original promotion", promoted_at),
        ("applied retreat", retreated_at),
    ] {
        assert!(
            cited.contains(&seq),
            "the restoration cites its {witness} {seq:?}: {cited:?}"
        );
    }
}

/// A budget generous enough for caps several steps deep.
fn deep() -> Params {
    Params {
        machine_budget: Rate::per_hour(1.0).unwrap(),
        ..Params::SPEC
    }
}

/// The committed slope's kind on `ccd`.
fn slope_kind(journal: &Journal, ccd: usize) -> String {
    serde_json::to_value(journal.state.project()).unwrap()["units"][ccd]["cap"]["slope"]["kind"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A promoted D, then a night whose CCD 0 mapping fails at -10 and deeper until CCD 0's slope is measured
/// without a justified decay, which holds its cap at CO 0.
fn measured() -> Journal {
    let (mut journal, _) = promoted_with(deep());
    journal.reboot("tuning1");
    while slope_kind(&journal, 0) == "fallback" {
        let plan = journal.until_trial(journal.now());
        let fails = plan.activity == Activity::Mapping
            && plan
                .profile
                .get(togi_tuner::UnitId::Ccd(ccd(0)))
                .is_some_and(|offset| offset.get() <= -10);
        journal.run(
            &plan,
            if fails {
                Ending::Fail(None)
            } else {
                Ending::Pass
            },
        );
    }
    assert_eq!(cap_of(&journal, 0), 0);
    let projected = journal.state.project();
    assert_eq!(
        projected.units[0].evidence.as_ref().unwrap().reason,
        togi_tuner::policy::CapReason::NoJustifiedDecay
    );
    assert!(
        !projected.units[0]
            .evidence
            .as_ref()
            .unwrap()
            .slope_inputs
            .is_empty()
    );
    journal
}

/// The witnesses an edge cites.
fn edge_witnesses(edge: &Edge) -> Vec<Seq> {
    match edge {
        Edge::Unknown {} => Vec::new(),
        Edge::Observed { witness, .. } => vec![*witness],
        Edge::Censored { witnesses, .. } => witnesses.clone(),
    }
}

/// Asserts every edge and cap committed after `changed` cites only what came after it.
fn cites_only_after(journal: &Journal, changed: Seq) {
    for event in journal.events.iter().filter(|event| event.seq > changed) {
        let edge = if let Payload::CcdEdge(committed) = &event.payload {
            &committed.edge
        } else if let Payload::CcdCap(committed) = &event.payload {
            &committed.edge
        } else {
            continue;
        };
        let witnesses = edge_witnesses(edge);
        assert!(
            witnesses.iter().all(|&witness| witness > changed),
            "{:?} cites {witnesses:?} from before {changed:?}",
            event.seq
        );
    }
}

#[test]
fn a_firmware_change_restarts_the_slopes_and_rebuilds_the_caps_from_new_evidence() {
    let mut journal = measured();
    let changed = journal.boot_start_at("day2", "2", &zero());
    let committed = journal.settle_day(&cold_boot_at("day2", journal.now()));
    let step = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::FirmwareZero { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no firmware zero in {committed:?}"));
    apply(&mut journal, &step, 2, "day2");
    journal.reboot("tuning2");
    journal.append(firmware("2").into(), &[]);
    for _ in 0..1_500 {
        let plan = journal.until_trial(journal.now());
        journal.run(&plan, Ending::Pass);
    }
    assert_eq!(slope_kind(&journal, 0), "fallback");
    assert!(cap_of(&journal, 0) < 0, "{}", cap_of(&journal, 0));
    assert!(cap_of(&journal, 1) < 0, "{}", cap_of(&journal, 1));
    cites_only_after(&journal, changed);
}

#[test]
fn a_rebuilt_backend_restarts_the_slopes_and_rebuilds_the_caps_from_new_evidence() {
    let mut journal = measured();
    let changed = journal.events.last().unwrap().seq;
    journal.rebuild(rebuilt());
    for _ in 0..1_500 {
        let plan = journal.until_trial(journal.now());
        journal.run(&plan, Ending::Pass);
    }
    assert_eq!(slope_kind(&journal, 0), "fallback");
    assert!(cap_of(&journal, 0) < 0, "{}", cap_of(&journal, 0));
    cites_only_after(&journal, changed);
}

#[test]
fn a_failure_is_warned_of_once_its_class_passes_deeper() {
    let mut journal = Journal::ready();
    // CCD 0's first descent step fails on a core of CCD 1: the step goes on, and nothing deeper has passed.
    let failed = loop {
        let plan = journal.until_trial(journal.now());
        if plan.activity == Activity::Mapping
            && matches!(plan.tested, Tested::Isolated { ccd: tested, offset } if tested == ccd(0) && offset.get() < 0)
        {
            break journal.run(&plan, Ending::Fail(Some(2)));
        }
        journal.run(&plan, Ending::Pass);
    };
    let warnings = |journal: &Journal| -> Vec<(Seq, Vec<Seq>, Vec<Seq>)> {
        journal
            .events
            .iter()
            .filter_map(|event| {
                if let Payload::TunerWarning(warning) = &event.payload
                    && warning.warning == WarningKind::Monotonicity
                {
                    Some((event.seq, event.cause.clone(), warning.passes.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    for _ in 0..3_000 {
        if !warnings(&journal).is_empty() {
            break;
        }
        let plan = journal.until_trial(journal.now());
        journal.run(&plan, Ending::Pass);
    }
    let warnings = warnings(&journal);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (_, causes, passes) = &warnings[0];
    assert!(causes.contains(&failed), "{causes:?}");
    assert!(!passes.is_empty());
    assert!(passes.iter().all(|&pass| pass > failed), "{passes:?}");
}

#[test]
fn a_cap_says_why_each_class_is_uncovered() {
    let mut journal = Journal::ready();
    journal.until_filler(|_| Ending::Pass);
    let first = journal
        .events
        .iter()
        .find_map(|event| {
            if let Payload::CcdCap(cap) = &event.payload
                && cap.ccd == ccd(0)
            {
                Some(cap)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("the night committed no cap for CCD 0"));
    let reasons = |regime: Regime| -> Vec<UncoveredReason> {
        first
            .uncovered
            .iter()
            .filter(|uncovered| uncovered.class.regime == regime)
            .map(|uncovered| uncovered.reason)
            .collect()
    };
    // R3 lies outside the mapping projection, so qualification exposure leaves it uncovered, and says so.
    let r3 = reasons(Regime::R3);
    assert!(!r3.is_empty());
    assert!(
        r3.iter()
            .all(|&reason| reason == UncoveredReason::NonMappingOnly),
        "{r3:?}"
    );
    // R9 is a mapping class the night had not mapped yet when it first committed a cap.
    let r9 = reasons(Regime::R9);
    assert!(r9.contains(&UncoveredReason::NoObservations), "{r9:?}");
}

/// The uncovered classes of each cap the first night commits, every trial passing. With `crash`, the
/// session is killed between the first R9 mapping trial's intent and its start, and the trial is rerun.
fn uncovered_by_cap(crash: bool) -> Vec<Vec<ExposureClass>> {
    let mut journal = Journal::ready();
    let mut crashed = !crash;
    for step in 0..3_500_i64 {
        let plan = journal.until_trial(at(100 + step));
        if plan.task.queue == Queue::Filler {
            break;
        }
        if !crashed
            && plan.activity == Activity::Mapping
            && matches!(plan.cell.roles, Roles::AllThreads { .. })
        {
            journal.run_unstarted(&plan);
            crashed = true;
        }
        journal.run(&plan, Ending::Pass);
    }
    assert!(crashed, "the night ran no R9 mapping trial");
    journal
        .events
        .iter()
        .filter_map(|event| {
            let Payload::CcdCap(cap) = &event.payload else {
                return None;
            };
            Some(
                cap.uncovered
                    .iter()
                    .map(|uncovered| uncovered.class.clone())
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn a_trial_killed_before_its_start_adds_no_class_to_the_caps() {
    let crashed = uncovered_by_cap(true);
    assert!(
        crashed
            .iter()
            .flatten()
            .all(|class| class.regime != Regime::R9 || class.position.is_some()),
        "{crashed:?}"
    );
    assert_eq!(crashed, uncovered_by_cap(false));
}

#[test]
fn read_only_projection_distinguishes_missing_mapping_exposure_from_unknown_edge() {
    let mut journal = Journal::ready();
    assert_eq!(
        journal.state.project().units[0]
            .evidence
            .as_ref()
            .unwrap()
            .reason,
        togi_tuner::policy::CapReason::NoAdequateClass
    );
    loop {
        let plan = journal.until_trial(journal.now());
        let mapping = plan.activity == Activity::Mapping;
        journal.run(&plan, Ending::Pass);
        if mapping {
            let _ = journal.until_trial(journal.now());
            break;
        }
    }
    let before = serde_json::to_value(&journal.state).unwrap();
    let projected = journal.state.project();
    assert_eq!(
        projected.units[0].evidence.as_ref().unwrap().reason,
        togi_tuner::policy::CapReason::UnknownEdge
    );
    let evidence = projected.units[0].evidence.as_ref().unwrap();
    assert!(!evidence.cells.is_empty());
    assert!(evidence.cells[0].eligible_s > 0);
    assert_eq!(evidence.cells.len(), evidence.projected.len());
    assert_eq!(serde_json::to_value(&journal.state).unwrap(), before);
}

#[test]
fn a_zero_exposure_interruption_adds_no_slope_input() {
    let evidence = |interrupted| {
        let mut journal = Journal::ready();
        loop {
            let plan = journal.until_trial(journal.now());
            if plan.activity == Activity::Mapping {
                if interrupted {
                    journal.run_unstarted(&plan);
                }
                journal.run(&plan, Ending::Pass);
                let _ = journal.until_trial(journal.now());
                return journal
                    .state
                    .project()
                    .units
                    .into_iter()
                    .map(|ccd| ccd.evidence)
                    .collect::<Vec<_>>();
            }
            journal.run(&plan, Ending::Pass);
        }
    };
    assert_eq!(evidence(true), evidence(false));
}

#[test]
fn qualification_projection_reports_measured_seconds_once_not_completed_cell_counts() {
    let (journal, profile) = Journal::promoted_compact();
    let projected = journal.state.project();
    let qualification = projected
        .qualifications
        .iter()
        .find(|record| record.profile.matches_ccd(&profile))
        .unwrap();
    assert_eq!(
        qualification.eligible_s,
        u64::from(togi_tuner::night::SEARCH_DURATION.get())
    );
    assert_ne!(qualification.eligible_s, 1);
}
