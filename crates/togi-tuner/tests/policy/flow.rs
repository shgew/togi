//! Day and night through the journal: a qualified proposal promoted and applied, the firmware zero and the
//! retained profile's re-qualification, an unexplained end's provisional veto and retreat, its one-night
//! reproduction, a reclassified step back's restoration, and a stock failure's dead end.

use std::collections::BTreeMap;

use test_case::test_case;

use togi_journal::{
    Activity, ApplicationId, ApplyCause, CcdVector, CellEvidence, Classification, ClimbAction,
    ClimbDecision, ClimbRetreatCause, DayApplied, DayCheckpoint, DayCleanStop, DayOffsetsChanged,
    DayUncleanEnd, Edge, LoadMix, NightQualification, Payload, QualificationPurpose,
    QualificationState, Queue, ReproductionState, Seq, Tested, WarningKind,
};
use togi_machine::{BootId, CcdProfile, EndEvidence, ResetReason};
use togi_tuner::day::{Invocation, Pause};
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

/// Expands a coarse decision back to the released day application's CCD vector.
pub(crate) fn profile(vector: &togi_journal::UnitVector) -> CcdProfile {
    CcdProfile::try_from(
        vector
            .mapping
            .units
            .iter()
            .map(|unit| {
                let togi_tuner::UnitId::Ccd(ccd) = unit.unit else {
                    panic!("coarse policy fixture contains a core unit");
                };
                let mut offsets = unit
                    .cores
                    .iter()
                    .map(|core| vector.cores.get(core).unwrap());
                let offset = offsets.next().unwrap();
                assert!(offsets.all(|member| member == offset));
                (ccd, offset)
            })
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap()
}

/// The steps among `payloads`.
pub(crate) fn steps(payloads: &[Payload]) -> Vec<&ClimbDecision> {
    payloads
        .iter()
        .filter_map(|payload| {
            if let Payload::ClimbDecision(step) = payload {
                Some(step)
            } else {
                None
            }
        })
        .collect()
}

/// Applies `step` in `boot` as the session would after its readback verified, returning the application's seq.
pub(crate) fn apply(
    journal: &mut Journal,
    step: &ClimbDecision,
    application: u64,
    boot: &str,
) -> Seq {
    let cause = journal
        .events
        .iter()
        .rev()
        .find(|event| matches!(&event.payload, Payload::ClimbDecision(committed) if committed == step))
        .map(|event| event.seq)
        .unwrap();
    journal.append(
        DayApplied {
            application: ApplicationId(application),
            cause: ApplyCause::Step {
                decision: togi_journal::DecisionId(cause.get()),
            },
            target: CcdVector::expand(profile(&step.proposed), &crate::fixture::cores()).unwrap(),
            readbacks: offsets_of(&profile(&step.proposed)),
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
    promoted_ending(params, |_| Ending::Pass)
}

fn promoted_ending(params: Params, ending: impl Fn(&TrialPlan) -> Ending) -> (Journal, CcdProfile) {
    let mut journal = Journal::ready_with(params);
    journal.hold_stock();
    let _ = journal.until_filler(ending);
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
        .find(|step| matches!(step.decision, ClimbAction::Promote { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no promotion in {committed:?}"));
    assert_eq!(crate::flow::profile(&step.proposed), qualified);
    apply(&mut journal, &step, 1, "day1");
    (journal, qualified)
}

#[test]
fn a_qualified_proposal_is_promoted_at_the_cold_boot_and_its_application_commits_d() {
    let (journal, qualified) = promoted();
    assert_eq!(
        qualified,
        zero().with(ccd(0), togi_machine::TargetOffset::new(-5).unwrap())
    );
    assert_eq!(
        journal.state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&qualified))
    );
}

/// The firmware scenario up to the applied restoration: the firmware update zeroes D, the operator
/// attests stock and the night re-qualifies retained D exactly, a stock hold passes, and D is
/// applied as application 3 in the boot `restoration`. Returns the journal, D and the applied restoration.
fn firmware_restored() -> (Journal, CcdProfile, ClimbDecision) {
    let (mut journal, d) = promoted();
    // The firmware update clears the offsets; the next cold boot reports the new firmware.
    journal.boot_start_at("day2", "2", &zero());
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let step = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.decision, ClimbAction::FirmwareZero { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no firmware zero in {committed:?}"));
    assert!(crate::flow::profile(&step.proposed).is_zero());
    apply(&mut journal, &step, 2, "day2");
    assert_eq!(
        journal.state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&zero()))
    );
    // The operator attests the new context; the first trial qualifies retained D, never stock.
    journal.reboot("tuning2");
    journal.append(firmware("2").into(), &[]);
    journal.attest(&firmware("2"));
    for step in 0..3_000_i64 {
        let plan = journal.until_trial(at(62_000 + step));
        if step == 0 {
            assert_eq!(plan.activity, Activity::Qualification, "{plan:?}");
            assert_eq!(plan.profile.to_ccd().unwrap(), d);
        }
        if qualifications(&journal).iter().any(|record| {
            record.purpose == QualificationPurpose::Firmware && *record.profile.ccd() == d
        }) {
            assert_eq!(plan.tested, Tested::Exact {});
            let _ = journal.until_filler(|_| Ending::Pass);
            assert!(qualifications(&journal).iter().any(|record| {
                record.purpose == QualificationPurpose::Firmware
                    && *record.profile.ccd() == d
                    && matches!(record.state, QualificationState::Complete { .. })
            }));
            journal.boot_start_at("before-hold", "2", &zero());
            let held = journal.settle_day(&cold_boot_at("before-hold", journal.now()));
            assert!(
                steps(&held)
                    .iter()
                    .all(|step| matches!(step.decision, ClimbAction::Hold { .. }))
            );
            journal.hold_stock_in("2");
            journal.boot_start_at("restoration", "2", &zero());
            let restored = journal.settle_day(&cold_boot_at("restoration", journal.now()));
            let restoration = step_of(&restored, |action| {
                matches!(
                    action,
                    ClimbAction::Restore {
                        restoration: togi_journal::RestorationKind::Firmware,
                        ..
                    }
                )
            });
            assert_eq!(profile(&restoration.proposed), d);
            assert_eq!(profile(&restoration.before), zero());
            apply(&mut journal, &restoration, 3, "restoration");
            assert_eq!(
                journal.state.project().applied,
                Some(togi_tuner::UnitProfile::of_ccd(&d))
            );
            return (journal, d, restoration);
        }
        journal.run(&plan, Ending::Pass);
    }
    panic!("D was never re-qualified under the new firmware");
}

#[test]
fn a_firmware_change_zeroes_d_requalifies_it_without_controls_and_restores_it_after_a_stock_hold() {
    let (journal, d, _) = firmware_restored();
    assert_eq!(
        journal.state.project().applied,
        Some(togi_tuner::UnitProfile::of_ccd(&d))
    );
}

#[test]
fn a_qualified_firmware_restoration_is_held_while_durably_paused() {
    let (mut journal, _, restoration) = firmware_restored();
    let before = journal.events.iter().position(|event| {
        matches!(&event.payload, Payload::ClimbDecision(decision) if *decision == restoration)
    }).unwrap();
    journal.events.truncate(before);
    journal.state = crate::climb::replay(&journal.events);
    journal.append(
        togi_journal::DayPause {
            state: togi_journal::PauseState::UntilBoot {},
            source: togi_journal::PauseSource::Command { uid: 1000 },
        }
        .into(),
        &[],
    );
    let committed = journal.settle_day(&cold_boot_at("restoration", journal.now()));
    assert!(steps(&committed).iter().any(|step| matches!(
        &step.decision,
        ClimbAction::Hold { blocked_by } if blocked_by.contains(&togi_journal::ClimbBlocker::Paused {})
    )));
    assert!(!steps(&committed).iter().any(|step| matches!(
        step.decision,
        ClimbAction::Promote { .. } | ClimbAction::Restore { .. }
    )));
    assert_eq!(
        crate::climb::replay(&journal.events).project(),
        journal.state.project()
    );
}

/// The firmware restoration commitment a settled day owes, if it committed one.
fn restoration_of(payloads: &[Payload]) -> Option<&togi_journal::FirmwareRestoration> {
    payloads.iter().find_map(|payload| {
        if let Payload::FirmwareRestoration(restoration) = payload {
            Some(restoration)
        } else {
            None
        }
    })
}

#[test]
fn a_successful_firmware_restoration_settles_as_restored_with_its_application() {
    let (mut journal, d, restoration) = firmware_restored();
    // Until the day settles it, the journal still records the restoration as owed, not as done.
    assert!(!matches!(
        journal
            .state
            .project()
            .climb
            .firmware_restoration
            .map(|saved| saved.state),
        Some(togi_journal::FirmwareRestorationState::Restored { .. })
    ));
    journal.boot_start_at("settled", "2", &d);
    let committed = journal.settle_day(&cold_boot_at("settled", journal.now()));
    let settled = restoration_of(&committed)
        .unwrap_or_else(|| panic!("no restoration commitment in {committed:?}"));
    assert_eq!(settled.saved, restoration.proposed);
    assert_eq!(
        settled.state,
        togi_journal::FirmwareRestorationState::Restored {
            application: ApplicationId(3)
        }
    );
    let projection = journal.state.project();
    assert_eq!(
        projection.climb.firmware_restoration.as_ref(),
        Some(settled)
    );
    assert_eq!(crate::climb::replay(&journal.events).project(), projection);
    // Settled once: the next cold boot owes nothing more.
    journal.boot_start_at("quiet", "2", &d);
    let quiet = journal.settle_day(&cold_boot_at("quiet", journal.now()));
    assert_eq!(restoration_of(&quiet), None);
}

#[test]
fn a_cpu_suspect_end_after_a_firmware_restoration_settles_it_as_stepped_back_with_its_event() {
    let (mut journal, d, restoration) = firmware_restored();
    unexplained_end_under(&mut journal, "2", "restoration", "stepped", &d);
    let committed = journal.settle_day(&cold_boot_at("stepped", journal.now()));
    let event = committed
        .iter()
        .find_map(|payload| {
            if let Payload::DayEvent(event) = payload {
                Some(event.event.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no event in {committed:?}"));
    let retreat = step_of(&committed, |action| {
        matches!(action, ClimbAction::Retreat { .. })
    });
    assert_eq!(profile(&retreat.proposed), zero());
    apply(&mut journal, &retreat, 4, "stepped");
    journal.boot_start_at("settled", "2", &zero());
    let settled = journal.settle_day(&cold_boot_at("settled", journal.now()));
    let settled = restoration_of(&settled)
        .unwrap_or_else(|| panic!("no restoration commitment in {settled:?}"))
        .clone();
    assert_eq!(settled.saved, restoration.proposed);
    assert_eq!(
        settled.state,
        togi_journal::FirmwareRestorationState::SteppedBack { event }
    );
    let projection = journal.state.project();
    assert_eq!(
        projection.climb.firmware_restoration.as_ref(),
        Some(&settled)
    );
    assert_eq!(crate::climb::replay(&journal.events).project(), projection);
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
        .find(|step| matches!(step.decision, ClimbAction::Retreat { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no retreat in {committed:?}"));
    let ClimbAction::Retreat { cause, .. } = &retreat.decision else {
        unreachable!();
    };
    assert!(
        matches!(cause, ClimbRetreatCause::Event { .. }),
        "{cause:?}"
    );
    assert!(crate::flow::profile(&retreat.proposed).is_zero());
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
        .find(|step| matches!(step.decision, ClimbAction::Retreat { .. }))
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
    let plan = (0..500)
        .find_map(|_| {
            let plan = journal.until_trial(journal.now());
            assert_eq!(plan.night, night);
            if plan.task.queue == Queue::Reproduction {
                Some(plan)
            } else {
                journal.run(&plan, Ending::Pass);
                None
            }
        })
        .unwrap_or_else(|| panic!("the interrupted reproduction never resumed"));
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
        let Ok(plan) = journal.until_end(at(64_000 + step), &BTreeMap::new()) else {
            break;
        };
        assert_ne!(plan.night, night);
        assert_ne!(plan.task.queue, Queue::Reproduction, "{plan:?}");
        journal.run(&plan, Ending::Pass);
    }
}

#[test]
fn exact_reproduction_failure_on_a_stock_core_stays_a_profile_failure_without_zero_trials() {
    let (mut journal, d) = promoted();
    unexplained_end(&mut journal, &d);
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    let retreat = step_of(&committed, |action| {
        matches!(action, ClimbAction::Retreat { .. })
    });
    apply(&mut journal, &retreat, 2, "day2");
    journal.reboot("tuning2");
    let from = journal.events.len();
    let plan = journal.until_trial(at(62_000));
    assert_eq!(plan.task.queue, Queue::Reproduction);
    assert_eq!(plan.tested, Tested::Exact {});
    assert_eq!(plan.profile.to_ccd().unwrap(), d);
    journal.run(&plan, Ending::Fail(Some(2)));
    night_until_filler(&mut journal, "tuning2");
    assert!(journal.events[from..].iter().any(|event| {
        matches!(&event.payload, Payload::ExposureObserved(observed)
            if observed.activity == Activity::Reproduction
                && observed.tested == Tested::Exact {}
                && observed.outcome == togi_journal::Outcome::Failure
                && observed.profile.ccd() == &d)
    }));
    assert!(journal.events[from..].iter().any(|event| {
        matches!(&event.payload, Payload::NightReproduction(record)
            if record.state == ReproductionState::Reproduced)
    }));
    assert!(journal.events[from..].iter().all(|event| {
        if let Payload::TrialIntent(intent) = &event.payload {
            intent.activity != Some(Activity::Control)
                && intent.profile.iter().any(|(_, offset)| offset.get() != 0)
        } else {
            true
        }
    }));
}

/// A released all-zero control recorded by an older policy, not a trial selected by this scheduler.
fn historical_stock_plan(journal: &mut Journal) -> TrialPlan {
    let mut plan = journal.until_trial(at(100));
    plan.activity = Activity::Control;
    plan.task.queue = Queue::Control;
    plan.tested = Tested::Exact {};
    plan.profile = togi_tuner::UnitProfile::of_ccd(&zero());
    plan.cell.purpose = togi_machine::Purpose::Exact;
    plan
}

#[test]
fn a_historical_conclusive_stock_failure_remains_a_dead_end() {
    let mut journal = Journal::ready();
    let plan = historical_stock_plan(&mut journal);
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
fn a_historical_stock_failure_with_owed_commitments_already_establishes_the_dead_end() {
    let mut journal = Journal::ready();
    let plan = historical_stock_plan(&mut journal);
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
                "day": unit["day"],
                "remaining": unit["remaining"],
            }))
            .collect::<Vec<_>>(),
        "day": projection["day"],
        "climb": projection["climb"],
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
    unexplained_end_under(journal, "1", ended, next, at_profile);
}

/// [`unexplained_end_of`] on a machine whose BIOS is `bios_version`.
fn unexplained_end_under(
    journal: &mut Journal,
    bios_version: &str,
    ended: &str,
    next: &str,
    at_profile: &CcdProfile,
) {
    journal.boot_start_at(next, bios_version, &zero());
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
        let Ok(plan) = journal.until_end(journal.now(), &BTreeMap::new()) else {
            return plans;
        };
        if plan.task.queue == Queue::Filler {
            return plans;
        }
        journal.run(&plan, Ending::Pass);
        plans.push(plan);
    }
    panic!("the night never reached its filler");
}

#[test]
fn a_fallback_to_co_zero_qualifies_the_wanted_profile_first_and_the_day_restores_it() {
    let params = Params::SPEC;
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
        .find(|step| matches!(step.decision, ClimbAction::Promote { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no second promotion in {committed:?}"));
    apply(&mut journal, &second, 2, "day2");
    // A month later the first D's exact certificate has lapsed. Its night ceiling
    // remains unchanged: diagnostic statistical caps never select a safety target.
    journal.advance(31 * 24 * 3_600);

    // An unexplained end at the second D steps back to the first, which unqualified is held at CO 0.
    unexplained_end_of(&mut journal, "day2", "day3", &profile(&second.proposed));
    let committed = journal.settle_day(&cold_boot_at("day3", journal.now()));
    let fallback = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.decision, ClimbAction::Retreat { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no retreat in {committed:?}"));
    let ClimbAction::Retreat { wanted, .. } = &fallback.decision else {
        unreachable!();
    };
    assert_eq!(wanted.as_ref().map(profile), Some(first.clone()));
    assert!(crate::flow::profile(&fallback.proposed).is_zero());
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
        .find(|step| crate::flow::profile(&step.proposed) == first)
        .unwrap_or_else(|| panic!("the wanted profile was never restored: {committed:?}"));
    assert!(
        matches!(
            restored.decision,
            ClimbAction::Retreat {
                cause: ClimbRetreatCause::Event { .. },
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
fn step_of(payloads: &[Payload], kind: impl Fn(&ClimbAction) -> bool) -> ClimbDecision {
    steps(payloads)
        .into_iter()
        .find(|step| kind(&step.decision))
        .cloned()
        .unwrap_or_else(|| panic!("no such step in {payloads:?}"))
}

#[test]
fn a_reclassified_step_back_commits_a_restoration_citing_its_answer_event_promotion_and_retreat() {
    let (mut journal, first) = promoted_with(Params::SPEC);
    held_at(&mut journal, &first);
    night_until_filler(&mut journal, "tuning1");
    journal.boot_start_at("day2", "1", &first);
    let committed = journal.settle_day(&cold_boot_at("day2", journal.now()));
    let promotion = step_of(&committed, |step| {
        matches!(step, ClimbAction::Promote { .. })
    });
    apply(&mut journal, &promotion, 2, "day2");
    let promoted_at = journal.events.last().unwrap().seq;

    // An unexplained end at the second D steps back to the qualified, nonzero first D.
    unexplained_end_of(&mut journal, "day2", "day3", &profile(&promotion.proposed));
    let committed = journal.settle_day(&cold_boot_at("day3", journal.now()));
    let retreat = step_of(&committed, |step| {
        matches!(step, ClimbAction::Retreat { .. })
    });
    assert_eq!(
        crate::flow::profile(&retreat.proposed),
        first,
        "{retreat:?}"
    );
    apply(&mut journal, &retreat, 3, "day3");
    let retreated_at = journal.events.last().unwrap().seq;
    held_at(&mut journal, &first);

    // A graphics answer reclassifies the end: the next cold boot owes the restoration.
    let answered_at = journal.append(
        togi_journal::DayAnswer {
            boot: BootId::from("day2".to_owned()),
            offsets: offsets_of(&profile(&promotion.proposed)),
            answer: togi_journal::DayAnswerKind::Graphics,
            uid: 1000,
        }
        .into(),
        &[],
    );
    journal.boot_start_at("day4", "1", &first);
    let committed = journal.settle_day(&cold_boot_at("day4", journal.now()));
    let restoration = step_of(&committed, |step| {
        matches!(
            step,
            ClimbAction::Restore {
                restoration: togi_journal::RestorationKind::Reclassified,
                ..
            }
        )
    });
    assert_eq!(restoration.before, retreat.proposed);
    assert_eq!(restoration.proposed, promotion.proposed);
    let ClimbAction::Promote { unit, .. } = promotion.decision else {
        panic!("expected an ordinary promotion");
    };
    assert!(
        matches!(&restoration.decision, ClimbAction::Restore { units, .. } if units == &[unit]),
        "the same unit is restored"
    );
    assert!(
        matches!(
            restoration.decision,
            ClimbAction::Restore {
                restoration: togi_journal::RestorationKind::Reclassified,
                ..
            }
        ),
        "{restoration:?}"
    );
    let event = journal
        .events
        .iter()
        .rev()
        .find(|event| matches!(&event.payload, Payload::DayEvent(_)))
        .map(|event| event.seq)
        .unwrap();
    assert_eq!(restoration.events, [event]);
    let cited = &journal
        .events
        .iter()
        .rev()
        .find(|committed| {
            matches!(&committed.payload, Payload::ClimbDecision(step) if *step == restoration)
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

/// A promoted D followed by isolated failures at two depths.
fn isolated_failures() -> Journal {
    let isolated = std::cell::RefCell::new(None);
    let (mut journal, _) = promoted_ending(Params::SPEC, |plan| {
        if matches!(plan.tested, Tested::Isolated { ccd: unit, .. } if unit == ccd(0))
            && isolated.borrow().is_none()
        {
            *isolated.borrow_mut() = Some(plan.clone());
        }
        Ending::Pass
    });
    let isolated = isolated.into_inner().unwrap();
    journal.reboot("slope-evidence");
    for depth in (10..=15).step_by(5) {
        let mut plan = isolated.clone();
        let offset = togi_machine::TargetOffset::new(-depth).unwrap();
        plan.task.queue = Queue::Mapping;
        plan.task.ordinal = u32::try_from(50_000 + depth).unwrap();
        plan.activity = Activity::Mapping;
        plan.tested = Tested::Isolated {
            ccd: ccd(0),
            offset,
        };
        plan.profile = togi_tuner::UnitProfile::of_ccd(&zero().with(ccd(0), offset));
        journal.run(&plan, Ending::Fail(None));
    }
    journal.until_trial(journal.now());
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

/// Asserts every edge committed after `changed` cites only what came after it.
fn cites_only_after(journal: &Journal, changed: Seq) {
    let mut edges = 0;
    for event in journal.events.iter().filter(|event| event.seq > changed) {
        let edge = if let Payload::CcdEdge(committed) = &event.payload {
            &committed.edge
        } else if let Payload::UnitEdge(committed) = &event.payload {
            &committed.new
        } else {
            continue;
        };
        edges += 1;
        let witnesses = edge_witnesses(edge);
        assert!(
            witnesses.iter().all(|&witness| witness > changed),
            "{:?} cites {witnesses:?} from before {changed:?}",
            event.seq
        );
    }
    assert!(edges > 0, "no rebuilt edges after {changed:?}");
}

#[test]
fn a_firmware_change_rebuilds_edges_from_new_evidence() {
    let mut journal = isolated_failures();
    let changed = journal.boot_start_at("day2", "2", &zero());
    let committed = journal.settle_day(&cold_boot_at("day2", journal.now()));
    let step = steps(&committed)
        .into_iter()
        .find(|step| matches!(step.decision, ClimbAction::FirmwareZero { .. }))
        .cloned()
        .unwrap_or_else(|| panic!("no firmware zero in {committed:?}"));
    apply(&mut journal, &step, 2, "day2");
    journal.reboot("tuning2");
    journal.append(firmware("2").into(), &[]);
    journal.attest(&firmware("2"));
    for _ in 0..1_500 {
        let Ok(plan) = journal.until_end(journal.now(), &BTreeMap::new()) else {
            break;
        };
        journal.run(&plan, Ending::Pass);
    }
    cites_only_after(&journal, changed);
}

#[test]
fn a_rebuilt_backend_rebuilds_edges_from_new_evidence() {
    let mut journal = isolated_failures();
    let changed = journal.events.last().unwrap().seq;
    journal.rebuild(rebuilt());
    for _ in 0..1_500 {
        let plan = journal.until_trial(journal.now());
        journal.run(&plan, Ending::Pass);
    }
    cites_only_after(&journal, changed);
}

#[test]
fn a_failure_is_warned_of_once_its_class_passes_deeper() {
    let mut journal = Journal::ready();
    // An isolated failure stops descent. A later recorded deeper pass still warns about
    // nonmonotonic raw evidence; the scheduler never probes through that failure to obtain it.
    let (failed, mut deeper) = loop {
        let plan = journal.until_trial(journal.now());
        if plan.activity == Activity::Mapping
            && matches!(plan.tested, Tested::Isolated { ccd: tested, offset } if tested == ccd(0) && offset.get() < 0)
        {
            break (journal.run(&plan, Ending::Fail(Some(2))), plan);
        }
        journal.run(&plan, Ending::Pass);
    };
    let offset = togi_machine::TargetOffset::new(-10).unwrap();
    deeper.task.ordinal = 50_000;
    deeper.tested = Tested::Isolated {
        ccd: ccd(0),
        offset,
    };
    deeper.profile = togi_tuner::UnitProfile::of_ccd(&zero().with(ccd(0), offset));
    journal.run(&deeper, Ending::Pass);
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
        let Ok(plan) = journal.until_end(journal.now(), &BTreeMap::new()) else {
            break;
        };
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
fn read_only_projection_does_not_change_state() {
    let mut journal = Journal::ready();
    let plan = journal.until_trial(journal.now());
    journal.run(&plan, Ending::Pass);
    let before = journal.state.clone();
    let _ = journal.state.project();
    assert_eq!(journal.state, before);
}

fn observed_mapping_trial(ending: Ending) -> (Journal, togi_journal::ExposureObserved) {
    let mut journal = Journal::ready();
    let plan = loop {
        let plan = journal.until_trial(journal.now());
        if plan.activity == Activity::Mapping {
            break plan;
        }
        journal.run(&plan, Ending::Pass);
    };
    journal.run(&plan, ending);
    loop {
        let action = journal.next(journal.now()).unwrap();
        let exposure = if let Action::Decide(decision) = &action
            && let PolicyPayload::ExposureObserved(observed) = &decision.payload
        {
            Some(observed.clone())
        } else {
            None
        };
        journal.commit(action);
        if let Some(exposure) = exposure {
            return (journal, exposure);
        }
    }
}

#[test_case(Ending::Inconclusive { interrupted: false }; "inconclusive observation")]
#[test_case(Ending::Inconclusive { interrupted: true }; "interrupted observation")]
#[test_case(Ending::Withheld; "withheld placement")]
fn an_unobserved_trial_adds_no_eligible_exposure(ending: Ending) {
    let (_, observed) = observed_mapping_trial(ending);
    assert_eq!(observed.eligible_s, 0);
    assert_eq!(observed.uninterrupted_s, 0);
    assert_eq!(observed.failures.len(), 0);
}

#[test_case(Ending::Fail(None), false; "live failure")]
#[test_case(Ending::Interrupted, true; "recovered failure")]
fn a_failed_trial_keeps_its_measured_exposure_and_failure(ending: Ending, interrupted: bool) {
    let (_, observed) = observed_mapping_trial(ending);
    let expected = observed.class.duration_s - observed.class.regime.warm_up_s();
    assert_eq!(observed.eligible_s, u64::from(expected));
    assert_eq!(
        observed.uninterrupted_s,
        if interrupted { 0 } else { u64::from(expected) },
    );
    assert_eq!(observed.failures.len(), 1);
}

#[test]
fn an_imported_inconclusive_exposure_preserves_the_raw_record() {
    let (_, mut observed) = observed_mapping_trial(Ending::Inconclusive { interrupted: true });
    observed.source.session = serde_json::from_str("\"20251231T000000Z\"").unwrap();
    observed.origin = togi_journal::ExposureOrigin::Imported {};
    observed.eligible_s = u64::from(observed.class.duration_s - observed.class.regime.warm_up_s());
    let mut journal = Journal::ready();
    journal.append(observed.clone().into(), &[]);
    let Payload::ExposureObserved(recorded) = &journal.events.last().unwrap().payload else {
        unreachable!();
    };
    assert_eq!(recorded, &observed, "the raw imported record is unchanged");
}

#[test]
fn an_interrupted_mapping_task_keeps_its_frozen_matrix() {
    let mut journal = Journal::ready();
    let plan = loop {
        let plan = journal.until_trial(journal.now());
        if matches!(plan.tested, Tested::Isolated { offset, .. } if offset.get() < -5) {
            break plan;
        }
        journal.run(&plan, Ending::Pass);
    };
    journal.run(&plan, Ending::Inconclusive { interrupted: true });
    let retry = journal.until_trial(journal.now());
    assert_eq!(retry.task, plan.task);
    assert_eq!(retry.cell, plan.cell);
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
