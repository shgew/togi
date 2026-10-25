//! Day and night through the journal: a qualified proposal promoted and applied, the firmware zero and the
//! retained profile's re-qualification, an unexplained end's provisional veto and retreat, its one-night
//! reproduction, and a stock failure's dead end.

use std::collections::BTreeMap;

use togi_journal::{
    Activity, ApplicationId, ApplyCause, Classification, DayApplied, DayStep, DayUncleanEnd,
    NightQualification, Payload, QualificationPurpose, QualificationState, Queue,
    ReproductionState, RetreatCause, StepKind, Tested,
};
use togi_machine::{BootId, CcdProfile, EndEvidence, ResetReason};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{DayQuery, EndReason};

use crate::fixture::{Ending, Journal, at, firmware, offsets_of, zero};

fn cold_boot(boot: &str, second: i64) -> DayQuery {
    DayQuery {
        boot: BootId::from(boot.to_owned()),
        invocation: Invocation::ColdBoot {
            pause: Pause::Running,
        },
        now: at(second),
    }
}

/// The steps among `payloads`.
fn steps(payloads: &[Payload]) -> Vec<&DayStep> {
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

/// Applies `step` in `boot` as the session would after its readback verified.
fn apply(journal: &mut Journal, step: &DayStep, application: u64, boot: &str) {
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
            readbacks: offsets_of(&step.proposed.ccd),
            transactions: Vec::new(),
            boot: BootId::from(boot.to_owned()),
            mono_ns: 1,
        }
        .into(),
        &[cause],
    );
}

/// The qualifications committed so far.
fn qualifications(journal: &Journal) -> Vec<&NightQualification> {
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
fn promoted() -> (Journal, CcdProfile) {
    let mut journal = Journal::ready();
    journal.until_filler(|_| Ending::Pass);
    let qualified = qualifications(&journal)
        .into_iter()
        .rev()
        .find(|record| {
            record.purpose == QualificationPurpose::NextStep
                && matches!(record.state, QualificationState::Complete { .. })
        })
        .map(|record| record.profile.ccd.clone())
        .unwrap_or_else(|| panic!("the night qualified no proposal"));
    journal.boot_start("day1");
    let committed = journal.settle_day(&cold_boot("day1", 60_000));
    let step = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.step, StepKind::Promote { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no promotion in {committed:?}"));
    assert_eq!(step.proposed.ccd, qualified);
    apply(&mut journal, &step, 1, "day1");
    (journal, qualified)
}

#[test]
fn a_qualified_proposal_is_promoted_at_the_cold_boot_and_its_application_commits_d() {
    let (journal, qualified) = promoted();
    assert!(!qualified.is_zero());
    assert_eq!(journal.state.project().applied, Some(qualified));
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
    assert!(step.proposed.ccd.is_zero());
    apply(&mut journal, &step, 2, "day2");
    assert_eq!(journal.state.project().applied, Some(zero()));
    // The tuning night runs the stock controls first, then re-qualifies the retained D exactly.
    journal.reboot("tuning2");
    journal.append(firmware("2").into(), &[]);
    for step in 0..3_000_i64 {
        let plan = journal.until_trial(at(62_000 + step));
        if step == 0 {
            assert_eq!(plan.activity, Activity::Control, "{plan:?}");
        }
        if qualifications(&journal).iter().any(|record| {
            record.purpose == QualificationPurpose::Firmware && record.profile.ccd == d
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
    assert!(retreat.proposed.ccd.is_zero());
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
    assert_eq!(plan.profile, d);
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

/// The projection after an unexplained end's provisional retreat, pinned except for the cap's class lists
/// and the night's task detail: the CCDs, the day, its event and veto, and the night that closed.
#[test]
fn the_projection_after_a_provisional_retreat_matches_its_snapshot() {
    let (mut journal, d) = promoted();
    unexplained_end(&mut journal, &d);
    journal.settle_day(&cold_boot("day2", 61_000));
    let projection = serde_json::to_value(journal.state.project()).unwrap();
    let pinned = serde_json::json!({
        "applied": projection["applied"],
        "ccds": projection["ccds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|ccd| serde_json::json!({
                "ccd": ccd["ccd"],
                "cores": ccd["cores"],
                "edge": ccd["edge"],
                "cap": ccd["cap"]["offset"],
                "binding": ccd["cap"]["binding"]["kind"],
                "day": ccd["day"],
                "remaining": ccd["remaining"],
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
