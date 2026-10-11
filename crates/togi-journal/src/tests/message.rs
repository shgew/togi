use togi_machine::Condition;

use super::fixtures::{core, sample, samples, target};
use crate::*;

fn message(kind: EventKind) -> String {
    sample(kind).message()
}

#[test]
fn every_kind_has_a_message() {
    for payload in samples() {
        assert!(!payload.message().is_empty(), "{}", payload.kind());
    }
}

#[test]
fn spec_quoted_texts() {
    let Payload::TunerDecision(mut decision) = sample(EventKind::TunerDecision) else {
        unreachable!()
    };
    assert_eq!(
        Payload::from(decision.clone()).message(),
        "core 07 passed R1+R2 at -32; next -37 (coarse, no failure point yet)"
    );
    decision.decision = Decision::Backoff;
    assert!(
        Payload::from(decision.clone())
            .message()
            .starts_with("core 07 failed at -32")
    );
    decision.activity = Activity::Checking;
    assert!(
        Payload::from(decision.clone())
            .message()
            .starts_with("core 07 backed off at -32")
    );
    decision.decision = Decision::Hold;
    decision.to_offset = decision.from_offset;
    assert!(
        Payload::from(decision)
            .message()
            .starts_with("core 07 holds at -32; next -32")
    );
}

#[test]
fn trial_end_pass_names_the_duration_and_temperature() {
    let Payload::TrialEnd(mut end) = sample(EventKind::TrialEnd) else {
        unreachable!()
    };
    end.outcome = Outcome::Pass;
    end.signal = None;
    end.core = None;
    end.duration_s = 90;
    end.voltage_request_median_v = None;
    end.voltage_request_min_v = None;
    end.top_requesters = togi_machine::CoreSet::default();
    assert_eq!(
        Payload::from(end).message(),
        "trial 0413 PASS 90 s | Tctl max 71°C"
    );
}

#[test]
fn trial_end_failure_says_what_it_knows() {
    let text = message(EventKind::TrialEnd);
    for part in [
        "trial 0413 FAIL computation_error on core 03",
        "last evidence 41 s after start",
        "last sample 40 s: Tctl 70°C, 5400-5600 MHz",
        "core 03 worker CPU time stopped advancing at 30500 ms after start (evidence only)",
        "loaded voltage request median 1.100 V, min 1.050 V",
        "top requester 03 1.125 V",
    ] {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
}

#[test]
fn trial_end_inconclusive_gives_the_reason() {
    let Payload::TrialEnd(mut end) = sample(EventKind::TrialEnd) else {
        unreachable!()
    };
    end.outcome = Outcome::Inconclusive;
    end.reason = Some("thermal trip".into());
    assert!(Payload::from(end).message().contains("INCONCLUSIVE"));
}

#[test]
fn intent_message_names_the_activity_fields() {
    let text = message(EventKind::TrialIntent);
    for part in [
        "trial 0413 core 03 CO -32 R2",
        "90 s alone",
        "(retry)",
        "cycle 2",
        "step 1",
        "hunt 3 group 4",
        "round 5",
        "rerun",
    ] {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
    let Payload::TrialIntent(mut intent) = sample(EventKind::TrialIntent) else {
        unreachable!()
    };
    intent.condition = Condition::Parked;
    intent.hunt = None;
    intent.group = None;
    assert!(
        Payload::from(intent)
            .message()
            .ends_with("rerun with every core at CO 0")
    );
}

#[test]
fn smu_messages_carry_core_and_offset() {
    assert_eq!(message(EventKind::SmuIntent), "SMU set core 03 to CO -20");
    assert_eq!(message(EventKind::SmuWrite), "SMU wrote all cores CO 0");
    assert_eq!(message(EventKind::SmuReadback), "SMU core 03 reads CO -20");
    let readback = SmuReadback {
        core: core(3),
        offset: togi_machine::ObservedOffset(-18),
        expected: Some(target(-20)),
    };
    assert_eq!(
        Payload::from(readback).message(),
        "SMU core 03 reads CO -18, expected -20"
    );
    assert!(message(EventKind::SmuError).contains("read core 03 failed: mailbox timeout"));
}

#[test]
fn shutdown_messages_follow_the_reason() {
    let Payload::Shutdown(mut shutdown) = sample(EventKind::Shutdown) else {
        unreachable!()
    };
    assert!(
        Payload::from(shutdown.clone())
            .message()
            .contains("requested 2 clean cycles")
    );
    shutdown.cycles = Some(std::num::NonZeroU32::MIN);
    assert!(
        Payload::from(shutdown.clone())
            .message()
            .contains("requested clean cycle; stopping")
    );
    shutdown.reason = ShutdownReason::FirstResult;
    assert!(Payload::from(shutdown).message().contains("first result"));
}

#[test]
fn crash_message_prefers_the_raw_reset_line() {
    let text = message(EventKind::CrashDetected);
    for part in [
        "boot e8f9aaaa",
        "in flight: seq 12",
        "stray",
        "reset reason: watchdog",
        "inconclusive",
        "pstore: /sys/fs/pstore/dmesg-1",
    ] {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
}

#[test]
fn mce_message_marks_scope_and_boot() {
    let text = message(EventKind::Mce);
    assert!(
        text.contains("uncorrected MCE on cpu 3 (core 03), bank 5 execution_unit (core-local)")
    );
    assert!(text.contains("from boot e8f9aaaa"));
}

#[test]
fn trial_signal_message_uses_the_schedule_or_the_counts() {
    assert!(message(EventKind::TrialSignal).contains("random on/off periods"));
    let Payload::TrialSignal(mut signal) = sample(EventKind::TrialSignal) else {
        unreachable!()
    };
    signal.schedule = None;
    assert!(
        Payload::from(signal)
            .message()
            .contains("4 stops, 3 continues")
    );
}

#[test]
fn hunt_messages_name_the_stage_and_result() {
    let Payload::HuntGroup(mut group) = sample(EventKind::HuntGroup) else {
        unreachable!()
    };
    assert!(Payload::from(group.clone()).message().contains("inferred"));
    group.inferred = None;
    group.probe = None;
    group.stage = HuntStage::Locate;
    assert!(Payload::from(group.clone()).message().contains("locate"));
    group.skipped = true;
    assert!(Payload::from(group).message().contains("skipped"));
    let Payload::HuntEnd(mut end) = sample(EventKind::HuntEnd) else {
        unreachable!()
    };
    end.result = HuntOutcome::Culprit;
    end.cores = [core(13)].into_iter().collect();
    assert_eq!(
        Payload::from(end).message(),
        "hunt 3 found core 13 after 4 groups (probed)"
    );
}

#[test]
fn combination_message_lists_the_members() {
    assert_eq!(
        message(EventKind::Combination),
        "combination C2: core 03 -40 + core 11 -30, fallback over every candidate of hunt 4"
    );
}

#[test]
fn carried_messages_name_the_source() {
    let text = message(EventKind::TrialCarried);
    assert!(text.contains("carried pass of trial 0413 from session 20261002T011407Z"));
    assert!(text.contains("evidence epoch 1"));
    let text = message(EventKind::FailureCarried);
    assert!(text.contains("carried idle failure from session 20261002T011407Z"));
    assert!(text.contains("source #819"));
}

#[test]
fn session_carried_message_counts_what_it_carries() {
    assert!(
        message(EventKind::SessionCarried)
            .starts_with("carried 1 candidate solo limits and 1 failure points")
    );
}

#[test]
fn units_appear_in_text() {
    assert!(message(EventKind::SessionContext).contains("boost limit 5000 MHz"));
    assert!(message(EventKind::BackendRetry).contains("after 300 s"));
    assert!(message(EventKind::JournalTorn).contains("2 torn bytes at offset 1024"));
}
