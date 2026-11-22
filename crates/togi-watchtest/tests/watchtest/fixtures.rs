use togi_journal::{
    Activity, DeadEndCondition, EventKind, Outcome, Payload, Queue, ShutdownReason,
};
use togi_machine::{BankType, Regime, ResetKind, TargetOffset};
use togi_watchtest::Base;

use crate::support::intent_of;

#[test]
fn the_night_base_runs_only_nonzero_work_until_the_operator_ends_it() {
    let events = Base::Night.events().expect("the base simulates");
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal
    ));
    let mut failures = 0_usize;
    for event in events {
        if let Payload::TrialIntent(intent) = &event.payload {
            assert!(
                matches!(
                    intent.activity,
                    Some(Activity::Mapping | Activity::Qualification)
                ),
                "#{}",
                event.seq.get()
            );
            assert!(
                intent.profile.iter().any(|(_, offset)| offset.get() != 0),
                "#{}",
                event.seq.get()
            );
        }
        if let Payload::TrialEnd(end) = &event.payload
            && end.outcome == Outcome::Failure
        {
            failures += 1;
            let failed = intent_of(events, &end.trial);
            assert!(
                failed.profile.iter().any(|(_, offset)| offset.get() != 0),
                "#{}: a failure at stock",
                event.seq.get()
            );
        }
        if let Payload::DeadEnd(dead) = &event.payload {
            assert_ne!(
                dead.condition,
                DeadEndCondition::FailureAtZero,
                "#{}",
                event.seq.get()
            );
        }
    }
    assert!(
        failures > 0,
        "the isolated computation error records a nonzero mapping failure"
    );
    let interrupted = events
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .expect("the signal ends a trial");
    assert_eq!(interrupted.outcome, Outcome::Inconclusive);
    assert!(interrupted.interrupted);
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::TrialStart(start) if start.trial == interrupted.trial)));
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::ProfileRestored(restored) if restored.offsets.len() == 16
            && restored.offsets.iter().all(|(_, offset)| offset == TargetOffset::ZERO))));
}

#[test]
fn the_dead_end_base_exhausts_nonzero_probes_without_testing_stock() {
    let events = Base::DeadEnd.events().expect("the base simulates");
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::CrashDetected)
    );
    assert!(events.iter().all(|event| !matches!(&event.payload,
        Payload::TrialIntent(intent) if intent.profile.iter().all(|(_, offset)| offset.get() == 0)
            || intent.activity == Some(Activity::Control))));
    let dead = events
        .iter()
        .find_map(|event| {
            if let Payload::DeadEnd(dead) = &event.payload {
                Some(dead)
            } else {
                None
            }
        })
        .expect("exhausted mapping ends with named no evidence");
    assert_eq!(dead.condition, DeadEndCondition::NoEvidence);
    assert!(dead.detail.starts_with("no_nonzero_work:"));
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::DeadEnd
    ));
}

#[test]
fn the_mapped_base_passes_r2_and_r6_before_the_scripted_mapping_crash() {
    let events = Base::Mapped.events().expect("the base simulates");
    let crash = events
        .iter()
        .position(|event| event.kind() == EventKind::CrashDetected)
        .expect("a crash is recorded");
    let before_crash = &events[..crash];
    assert!(before_crash.iter().any(|event| matches!(&event.payload,
        Payload::ExposureObserved(exposure)
            if exposure.class.regime == Regime::R2 && exposure.activity == Activity::Mapping
                && exposure.outcome == Outcome::Pass && exposure.eligible_s > 0)));
    assert!(before_crash.iter().any(|event| matches!(&event.payload,
        Payload::TrialEnd(end) if end.outcome == Outcome::Pass
            && intent_of(before_crash, &end.trial).regime == Regime::R6)));
    let Payload::CrashDetected(detected) = &events[crash].payload else {
        unreachable!("the position is a crash.detected");
    };
    let flight = detected.in_flight.expect("a trial was in flight");
    let Payload::TrialIntent(intent) =
        &events[usize::try_from(flight.get() - 1).expect("an index")].payload
    else {
        panic!("the in-flight seq names a trial intent");
    };
    assert_eq!(intent.task.map(|task| task.queue), Some(Queue::Mapping));
    assert_eq!(detected.reset_reason, Some(ResetKind::SyncFlood));
    let start = before_crash
        .iter()
        .find_map(|event| {
            if let Payload::TrialStart(start) = &event.payload
                && start.trial == intent.trial
            {
                Some(start)
            } else {
                None
            }
        })
        .expect("the crashed trial actually ran");
    let mces: Vec<_> = before_crash
        .iter()
        .filter_map(|event| {
            if let Payload::Mce(mce) = &event.payload {
                Some((event.seq, mce))
            } else {
                None
            }
        })
        .collect();
    assert!(
        mces.iter().any(|(seq, mce)| !mce.corrected
            && mce.bank_type == BankType::LoadStore
            && mce.from_boot.as_ref() == Some(&events[crash].boot)
            && events[crash].cause.contains(seq)
            && start.cpus.contains(&mce.cpu)),
        "MCEs {mces:?}, cited {:?}, CPUs {:?}",
        events[crash].cause,
        start.cpus
    );
    assert!(
        events[crash + 1..]
            .iter()
            .all(|event| event.kind() != EventKind::TrialIntent),
        "the base halts once the crash is recorded"
    );
}

#[test]
fn the_filler_base_halts_at_its_first_nonzero_filler_trial_start() {
    let events = Base::Filler.events().expect("the base simulates");
    let fillers: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload
                && intent.task.is_some_and(|task| task.queue == Queue::Filler)
            {
                Some(intent.trial.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(fillers.len(), 1, "{fillers:?}");
    assert!(matches!(&events.last().expect("events").payload,
        Payload::TrialStart(start) if start.trial == fillers[0]));
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::TrialIntent(intent) if intent.trial == fillers[0] && intent.profile.iter().any(|(_, offset)| offset.get() != 0))));
}
