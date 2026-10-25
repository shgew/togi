use test_case::test_case;
use togi_machine::{Condition, CoreSet, PerCore, Regime, Signal, SleepMode};

use super::fixtures::{
    core, failure, member, ord, profile, sample, samples, seq, set, target, trial,
};
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
        "core 07 passed R1+R2 at -32; searches -37 on its next turn (coarse, no failure point yet)"
    );
    decision.decision = Decision::Backoff;
    assert!(
        Payload::from(decision.clone())
            .message()
            .starts_with("core 07 failed at -32; searches -37 on its next turn")
    );
    decision.activity = Activity::Checking;
    assert!(
        Payload::from(decision.clone())
            .message()
            .starts_with("core 07 backed off from -32 to -37")
    );
    decision.decision = Decision::Hold;
    decision.to_offset = decision.from_offset;
    assert!(
        Payload::from(decision)
            .message()
            .starts_with("core 07 failure held; tuning stays at -32 (")
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
fn trial_end_records_host_exclusion_without_claiming_whole_load_coverage() {
    let mut end = bare_trial_end(Outcome::Pass, None, "");
    assert_eq!(end.evidence_outcome(), Outcome::Pass);
    assert_eq!(serde_json::to_value(&end).unwrap().get("excluded"), None);
    end.excluded = vec![core(3)];
    let encoded = serde_json::to_value(&end).unwrap();
    assert_eq!(encoded["excluded"], serde_json::json!([3]));
    let decoded: TrialEnd = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, end);
    assert_eq!(end.evidence_outcome(), Outcome::Inconclusive);
    assert!(
        Payload::from(end)
            .message()
            .contains("no exposure credit for core 03; whole-load coverage withheld")
    );
}

#[test_case(Outcome::Failure; "failures retain their safety evidence")]
#[test_case(Outcome::Inconclusive; "inconclusive stays inconclusive")]
fn trial_end_exclusion_never_changes_nonpassing_evidence(outcome: Outcome) {
    let mut end = bare_trial_end(outcome, None, "");
    end.excluded = vec![core(3)];
    assert_eq!(end.evidence_outcome(), outcome);
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

fn bare_trial_end(outcome: Outcome, signal: Option<Signal>, reason: &str) -> TrialEnd {
    let Payload::TrialEnd(mut end) = sample(EventKind::TrialEnd) else {
        unreachable!()
    };
    end.outcome = outcome;
    end.signal = signal;
    end.core = None;
    end.reason = Some(reason.into());
    end.duration_s = 990;
    end.last_evidence = false;
    end.tctl_max_c = None;
    end.voltage_request_median_v = None;
    end.voltage_request_min_v = None;
    end.voltage_requests_v = PerCore::default();
    end.top_requesters = CoreSet::default();
    end.stalled_core = None;
    end.worker_stalled_ms = None;
    end.last_sample_s = Some(987);
    end.last_sample_tctl_c = Some(73);
    end.last_sample_min_mhz = Some(4321);
    end.last_sample_max_mhz = Some(5432);
    end
}

#[test]
fn trial_end_inconclusive_gives_the_reason() {
    let Payload::TrialEnd(mut end) = sample(EventKind::TrialEnd) else {
        unreachable!()
    };
    end.outcome = Outcome::Inconclusive;
    end.reason = Some("thermal trip".into());
    let text = Payload::from(end.clone()).message();
    assert!(text.contains("INCONCLUSIVE"), "{text}");
    assert!(text.contains(": thermal trip"), "{text}");
    assert!(!text.contains("FAIL"), "{text}");
    end.reason = None;
    assert!(
        Payload::from(end)
            .message()
            .contains(": no reason recorded")
    );
}

#[test_case("crash", Some(Signal::Crash), Outcome::Failure, ", last evidence 990 s after start"; "crash")]
#[test_case("computation error", Some(Signal::ComputationError), Outcome::Failure, " after 990 s"; "computation error")]
#[test_case("thermal trip", None, Outcome::Inconclusive, " after 990 s"; "thermal trip")]
#[test_case("power loss", None, Outcome::Inconclusive, " after 990 s"; "power loss")]
fn recovered_trial_messages_keep_the_reason_and_the_last_sample(
    reason: &str,
    signal: Option<Signal>,
    outcome: Outcome,
    duration: &str,
) {
    let telemetry = "last sample 987 s: Tctl 73°C, 4321-5432 MHz";
    let text = Payload::from(bare_trial_end(outcome, signal, reason)).message();
    assert!(text.starts_with("trial 0413 "), "{text}");
    assert!(text.contains(duration), "{reason}: {text}");
    assert!(text.contains(telemetry), "{reason}: {text}");
    match outcome {
        Outcome::Inconclusive => {
            assert!(text.contains("INCONCLUSIVE"), "{text}");
            assert!(text.ends_with(&format!(": {reason}")), "{text}");
        }
        Outcome::Failure => assert!(text.contains("FAIL"), "{text}"),
        Outcome::Pass => unreachable!("no case recovers a passing trial: {text}"),
    }
}

#[test]
fn intent_message_names_the_activity_fields() {
    let text = message(EventKind::TrialIntent);
    for part in [
        "trial 0413 core 03 offset -32 R2",
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
            .ends_with("rerun with every core at offset 0")
    );
}

#[test]
fn smu_messages_carry_core_and_offset() {
    assert_eq!(
        message(EventKind::SmuIntent),
        "SMU set core 03 to offset -20"
    );
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
    assert_eq!(
        Payload::from(group.clone()).message(),
        "hunt 3 group 4: core 01 at -22 with core 00 -40 failure inferred: inferred"
    );
    group.inferred = None;
    group.probe = None;
    group.stage = HuntStage::Locate;
    group.reason = Some("locating".into());
    assert_eq!(
        Payload::from(group.clone()).message(),
        "hunt 3 group 4: locate with every unloaded core at 0 and the loaded cores at their failing offsets; trials of 120 s; locating"
    );
    group.stage = HuntStage::Part;
    group.cores = set(&[7]);
    group.skipped = true;
    group.reason = Some("already checked".into());
    let skipped = Payload::from(group).message();
    assert_eq!(skipped, "hunt 3 group 4: cores 07 skipped: already checked");
    assert!(!skipped.contains("trials of"), "{skipped}");
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

fn multi_core_intent() -> TrialIntent {
    let Payload::TrialIntent(mut intent) = sample(EventKind::TrialIntent) else {
        unreachable!()
    };
    intent.core = None;
    intent.offset = None;
    intent.cores = set(&[3, 7]);
    intent.retry = false;
    intent.activity = Some(Activity::Hunt);
    intent.night = None;
    intent.task = None;
    intent.content = Vec::new();
    intent.placement = None;
    intent.tested = None;
    intent.cycle = None;
    intent.step = None;
    intent.round = None;
    intent
}

#[test]
fn multi_core_trial_intents_name_every_loaded_core() {
    let hunt = TrialIntent {
        task_start: None,
        condition: Condition::Parked,
        ..multi_core_intent()
    };
    let text = Payload::from(hunt).message();
    assert_eq!(
        text,
        "trial 0413 cores 03, 07 R2 mprime AVX2 36K-248K 90 s parked hunt 3 group 4 rerun"
    );
    assert!(!text.contains(" offset "), "{text}");

    let deepening = TrialIntent {
        task_start: None,
        condition: Condition::Together,
        hunt: None,
        group: None,
        round: Some(ord(2)),
        ..multi_core_intent()
    };
    let text = Payload::from(deepening).message();
    assert!(text.starts_with("trial 0413 cores 03, 07 R2"), "{text}");
    assert!(text.ends_with("together round 2 rerun"), "{text}");
    assert!(!text.contains("hunt"), "{text}");
}

#[test]
fn a_single_target_trial_intent_says_core_not_cores() {
    let one = TrialIntent {
        task_start: None,
        condition: Condition::Parked,
        cores: set(&[5]),
        ..multi_core_intent()
    };
    let text = Payload::from(one).message();
    assert!(text.starts_with("trial 0413 core 05 R2"), "{text}");
    assert!(!text.contains("cores"), "{text}");
}

#[test]
fn checking_step_names_cycle_step_and_current_profile() {
    let text = Payload::from(CheckingStep {
        cycle: ord(2),
        step: ord(4),
        profile: profile(&[-20, -50]),
    })
    .message();
    assert!(
        text.starts_with("checking cycle 2 step 4 starts at current profile "),
        "{text}"
    );
    assert!(text.contains("-20") && text.contains("-50"), "{text}");
    assert!(!text.contains("R7"), "{text}");
}

#[test]
fn a_dead_end_shutdown_says_it_stopped_at_a_dead_end() {
    let Payload::Shutdown(mut shutdown) = sample(EventKind::Shutdown) else {
        unreachable!()
    };
    shutdown.reason = ShutdownReason::DeadEnd;
    let text = Payload::from(shutdown).message();
    assert_eq!(text, "stopped at a dead end");
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
            .starts_with("carried 1 candidate solo limit and 1 failure point from")
    );
    let Payload::SessionCarried(mut carried) = sample(EventKind::SessionCarried) else {
        unreachable!()
    };
    let mut second = carried.carried[0].clone();
    second.core = core(4);
    carried.carried.push(second);
    assert!(
        Payload::from(carried)
            .message()
            .starts_with("carried 2 candidate solo limits and 2 failure points from")
    );
}

#[test]
fn units_appear_in_text() {
    assert!(message(EventKind::SessionContext).contains("boost limit 5000 MHz"));
    assert!(message(EventKind::BackendRetry).contains("after 300 s"));
    assert!(message(EventKind::JournalTorn).contains("2 torn bytes at offset 1024"));
}

#[test_case(
    EventKind::DayBootStart,
    "day observer started by togi 0.1.0+abc1234: BIOS F31 on X570, 5950X, microcode 0xa201016, boost limit 5000 MHz; offsets [00: 0, 01: -5]; hardware watchdog armed: watchdog0 (SP5100 TCO timer) active, held by PID 1"
)]
#[test_case(
    EventKind::DayCheckpoint,
    "day observer has been active 3600 s in total this boot"
)]
#[test_case(
    EventKind::DayOffsetsChanged,
    "offsets read back changed to [00: -10, 01: -5]; the time since the previous record is not credited to any offset profile"
)]
#[test_case(EventKind::DayCleanStop, "day observer stopped on request")]
#[test_case(
    EventKind::DayUncleanEnd,
    "day observer of boot 7a1b2c3d ended without a clean stop; last checkpoint seq 7; last offsets [00: -10, 01: -5]; reset reason: watchdog; machine checks: none in that boot, unknown in the next; kernel log tail: amdgpu timeout; logging continued after the last error"
)]
#[test_case(
    EventKind::DayAnswer,
    "uid 1000 answered that boot 7a1b2c3d ended because graphics died but sound went on; offsets [00: -10, 01: -5]"
)]
#[test_case(
    EventKind::DayRecord,
    "recorded a crash at 2026-10-19T11:00:00Z that no day observer saw with offsets [00: -25, 01: -5] (entered in firmware setup at 2026-10-19T12:00:00Z); running: builds or tests; a Nix build"
)]
#[test_case(
    EventKind::DayFault,
    "uncorrected MCE on cpu 2 (core 01), bank 0 load_store at 1 s since boot; offsets [00: -10, 01: -5]"
)]
fn day_messages_tell_a_stranger_what_the_observer_saw(kind: EventKind, text: &str) {
    assert_eq!(message(kind), text);
}

#[test_case(
    SleepMode::Suspend,
    "; kernel log tail: amdgpu timeout; logging continued after the last error; the log ended at a suspend entry"
)]
#[test_case(
    SleepMode::Hibernate,
    "; kernel log tail: amdgpu timeout; logging continued after the last error; the log ended at a hibernation entry"
)]
fn an_unclean_end_that_ended_asleep_says_which_sleep(asleep: SleepMode, suffix: &str) {
    let Payload::DayUncleanEnd(mut end) = sample(EventKind::DayUncleanEnd) else {
        unreachable!()
    };
    end.evidence.asleep = Some(asleep);
    let text = Payload::from(end).message();
    assert!(text.ends_with(suffix), "{text}");
}

#[test_case(
    crate::LiveFault::KernelError { error: togi_machine::KernelErrorKind::RcuStall },
    "RCU stall at 1 s since boot; offsets [00: -10, 01: -5]"
; "kernel error")]
#[test_case(
    crate::LiveFault::Trap {
        trap: crate::Trap::IllegalInstruction,
        name: "mprime".into(),
        pid: 991,
        cpu: Some(togi_machine::CpuId::from(3_u16)),
        core: None,
    },
    "SIGILL (invalid opcode) in mprime[991] on cpu 3 at 1 s since boot; offsets [00: -10, 01: -5]"
; "trap")]
#[test_case(
    crate::LiveFault::MachineCheck {
        cpu: togi_machine::CpuId::from(5_u16),
        core: None,
        bank: 2,
        bank_type: togi_machine::BankType::L3Cache,
        corrected: true,
    },
    "corrected MCE on cpu 5 (no core), bank 2 l3_cache at 1 s since boot; offsets [00: -10, 01: -5]"
; "corrected machine check")]
#[test_case(
    crate::LiveFault::KernelError { error: togi_machine::KernelErrorKind::Oops },
    "kernel oops at 1 s since boot; offsets [00: -10, 01: -5]"
; "oops")]
#[test_case(
    crate::LiveFault::KernelError { error: togi_machine::KernelErrorKind::SoftLockup },
    "soft lockup at 1 s since boot; offsets [00: -10, 01: -5]"
; "soft lockup")]
#[test_case(
    crate::LiveFault::Trap {
        trap: crate::Trap::GeneralProtection,
        name: "togi".into(),
        pid: 421,
        cpu: Some(togi_machine::CpuId::from(8_u16)),
        core: Some(togi_machine::CoreId::new(0).unwrap()),
    },
    "general-protection fault in togi[421] on cpu 8 (core 00) at 1 s since boot; offsets [00: -10, 01: -5]"
; "general protection")]
#[test_case(
    crate::LiveFault::Trap {
        trap: crate::Trap::Bus,
        name: "mprime".into(),
        pid: 7,
        cpu: None,
        core: None,
    },
    "SIGBUS in mprime[7] at 1 s since boot; offsets [00: -10, 01: -5]"
; "bus error")]
fn live_fault_messages_name_the_fault_the_time_and_the_profile(
    fault: crate::LiveFault,
    text: &str,
) {
    let Payload::DayFault(mut sample) = sample(EventKind::DayFault) else {
        unreachable!()
    };
    sample.fault = fault;
    assert_eq!(Payload::from(sample).message(), text);
}

#[test]
fn a_checkpoint_with_a_load_mix_names_each_share_in_seconds() {
    let Payload::DayCheckpoint(mut checkpoint) = sample(EventKind::DayCheckpoint) else {
        unreachable!()
    };
    checkpoint.load_mix = crate::LoadMix {
        single_thread_ms: 1500,
        all_threads_ms: 62_000,
        ..crate::LoadMix::default()
    };
    assert_eq!(
        Payload::from(checkpoint).message(),
        "day observer has been active 3600 s in total this boot; sampled load: 1 s single-thread, 0 s light beside busy cores, 62 s all threads, 0 s idle transitions, 0 s quiet, 0 s other"
    );
}

#[test_case(true; "counting")]
#[test_case(false; "open but not counting")]
fn checkpoints_record_and_describe_the_latest_watchdog_check(counting: bool) {
    let detail = "watchdog0 open but not counting: timeleft 4294967295 s exceeds timeout 60 s";
    let watchdog = if counting {
        togi_machine::WatchdogState::Armed {
            detail: "watchdog0 counting, held by PID 1".into(),
        }
    } else {
        togi_machine::WatchdogState::Unarmed {
            detail: detail.into(),
        }
    };
    let checkpoint = crate::DayCheckpoint {
        applied_secs: 600,
        load_mix: crate::LoadMix::default(),
        watchdog: Some(watchdog.clone()),
    };
    let encoded = serde_json::to_value(&checkpoint).unwrap();
    assert_eq!(
        encoded["watchdog"],
        serde_json::to_value(&watchdog).unwrap()
    );
    assert_eq!(
        serde_json::from_value::<crate::DayCheckpoint>(encoded).unwrap(),
        checkpoint
    );
    let armed = if counting { "armed" } else { "not armed" };
    assert_eq!(
        Payload::from(checkpoint).message(),
        format!(
            "day observer has been active 600 s in total this boot; hardware watchdog {armed}: {}",
            watchdog.detail()
        )
    );
}

#[test]
fn an_older_checkpoint_has_no_watchdog_check() {
    let checkpoint: crate::DayCheckpoint = serde_json::from_str(r#"{"applied_secs":600}"#).unwrap();
    assert_eq!(checkpoint.watchdog, None);
    assert_eq!(
        serde_json::to_value(checkpoint).unwrap(),
        serde_json::json!({"applied_secs":600,"load_mix":{
            "single_thread_ms":0,"light_beside_busy_ms":0,"all_threads_ms":0,
            "idle_transition_ms":0,"quiet_ms":0,"other_ms":0
        }})
    );
}

fn hunt_end(result: HuntOutcome, cores: &[u8], groups: u32, reason: &str) -> String {
    Payload::from(HuntEnd {
        hunt: ord(3),
        result,
        cores: set(cores),
        members: Vec::new(),
        groups,
        reason: reason.into(),
    })
    .message()
}

#[test]
fn only_a_culprit_hunt_end_says_it_found_the_core() {
    assert_eq!(
        hunt_end(HuntOutcome::Culprit, &[7], 4, "solo limit alone"),
        "hunt 3 found core 07 after 4 groups (solo limit alone)"
    );
    assert_eq!(
        hunt_end(HuntOutcome::Culprit, &[7], 1, ""),
        "hunt 3 found core 07 after 1 group"
    );
    for result in HuntOutcome::ALL {
        if *result != HuntOutcome::Culprit {
            let text = hunt_end(*result, &[7], 1, "why");
            assert!(!text.contains("found core"), "{result}: {text}");
            assert!(text.contains("why"), "{result}: {text}");
        }
    }
}

#[test]
fn a_cancelled_hunt_end_establishes_no_culprit() {
    let text = hunt_end(HuntOutcome::Cancelled, &[7], 0, "core 07 was reset");
    assert!(
        text.starts_with("hunt 3 cancelled after 0 groups"),
        "{text}"
    );
    assert!(text.contains("no culprit"), "{text}");
    assert!(text.contains("core 07 was reset"), "{text}");
    assert!(!text.contains("found"), "{text}");
}

#[test]
fn a_direct_hunt_end_names_the_directly_attributed_core() {
    assert_eq!(
        hunt_end(HuntOutcome::Direct, &[7], 1, "worker stalled"),
        "hunt 3 ended after 1 group: core 07 was attributed directly (worker stalled)"
    );
}

#[test]
fn a_loaded_hunt_end_keeps_the_failure_on_the_loaded_cores() {
    assert_eq!(
        hunt_end(HuntOutcome::Loaded, &[0, 1], 1, "locate failed"),
        "hunt 3 ended after 1 group: the failure stays with loaded cores 00, 01 (locate failed)"
    );
}

#[test]
fn combination_and_fallback_hunt_ends_name_every_core_without_a_culprit() {
    let Payload::HuntEnd(mut end) = sample(EventKind::HuntEnd) else {
        unreachable!()
    };
    end.result = HuntOutcome::Combination;
    end.members = vec![member(0, -40), member(1, -30)];
    let text = Payload::from(end.clone()).message();
    assert!(text.contains("failing offset combination"), "{text}");
    assert!(text.contains("parked"), "{text}");
    assert!(!text.contains("alone"), "{text}");
    assert!(!text.contains("single core"), "{text}");
    assert!(text.contains("core 00 -40 + core 01 -30"), "{text}");
    assert!(text.contains("probed"), "{text}");
    assert!(!text.contains("found"), "{text}");
    end.result = HuntOutcome::Fallback;
    end.members.clear();
    let text = Payload::from(end).message();
    assert!(text.contains("falling back to every candidate"), "{text}");
    assert!(text.contains("cores 00, 01"), "{text}");
    assert!(!text.contains("found"), "{text}");
}

fn failure_message(failure: Failure) -> String {
    Payload::from(failure).message()
}

#[test]
fn an_unattributed_failure_with_a_trial_and_no_condition_names_the_trial() {
    let text = failure_message(Failure {
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        condition: None,
        ..failure()
    });
    assert!(text.contains("trial 0413"), "{text}");
    assert!(text.contains("R2"), "{text}");
    assert!(text.contains("no evidence names a single core"), "{text}");
    assert!(!text.contains("no trial was in flight"), "{text}");
    assert!(!text.contains("together"), "{text}");
}

#[test]
fn an_unattributed_failure_with_no_trial_and_no_condition_says_no_trial_was_in_flight() {
    let text = failure_message(Failure {
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        trial: None,
        condition: None,
        ..failure()
    });
    assert_eq!(text, "unattributed crash failure: no trial was in flight");
}

#[test]
fn an_idle_failure_with_an_alone_condition_does_not_invent_a_trial() {
    let text = failure_message(Failure {
        trial: None,
        condition: Some(Condition::Alone),
        ..failure()
    });
    assert!(text.contains("core 03 failure at CO -30"), "{text}");
    assert!(text.contains("no trial recorded"), "{text}");
    assert!(!text.contains("in trial"), "{text}");
    assert!(!text.contains("alone: attributed"), "{text}");
}

#[test]
fn an_attributed_alone_trial_failure_says_the_target_is_to_blame() {
    let text = failure_message(failure());
    assert!(text.contains("core 03 failure at CO -30"), "{text}");
    assert!(text.contains("in alone R2 trial 0413"), "{text}");
    assert!(text.contains("alone: attributed to the target"), "{text}");
}

#[test]
fn an_attributed_failure_with_a_trial_and_no_condition_does_not_claim_alone() {
    let text = failure_message(Failure {
        condition: None,
        ..failure()
    });
    assert!(text.contains("in R2 trial 0413"), "{text}");
    assert!(!text.contains("alone"), "{text}");
}

#[test]
fn an_idle_failure_with_a_loaded_condition_names_the_applied_profile() {
    let idle = Failure {
        core: Some(core(7)),
        trial: None,
        regime: Some(Regime::R6),
        condition: Some(Condition::Together),
        ..failure()
    };
    let text = failure_message(idle.clone());
    assert_eq!(
        text,
        "core 07 failure at CO -30: crash with the current profile applied and no trial in flight, the only nonzero core"
    );
    let text = failure_message(Failure {
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        ..idle.clone()
    });
    assert!(text.contains("no trial in flight"), "{text}");
    assert!(text.contains("counted as a R6 failure"), "{text}");
    let text = failure_message(Failure {
        condition: Some(Condition::Parked),
        ..idle
    });
    assert!(text.contains("the group profile applied"), "{text}");
}

#[test]
fn trial_failures_in_loaded_conditions_name_the_condition() {
    let unattributed = Failure {
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        regime: Some(Regime::R7),
        signal: Signal::Stall,
        ..failure()
    };
    assert_eq!(
        failure_message(Failure {
            condition: Some(Condition::Parked),
            ..unattributed.clone()
        }),
        "unattributed stall failure in parked R7 trial 0413: the group fails, no evidence names a single core"
    );
    assert_eq!(
        failure_message(Failure {
            condition: Some(Condition::Together),
            ..unattributed
        }),
        "unattributed stall failure in together R7 trial 0413: no evidence names a single core"
    );
}

#[test]
fn a_known_failure_skip_says_why_it_was_skipped() {
    let known = Failure {
        known_failure: Some(seq(12)),
        reason: Some("known failure #12 answers this scheduled class".into()),
        ..failure()
    };
    assert_eq!(
        failure_message(known.clone()),
        "known failure #12 answers this scheduled class"
    );
    assert_eq!(
        failure_message(Failure {
            reason: None,
            ..known
        }),
        "known failure activated"
    );
}

#[test]
fn a_carried_failure_keeps_the_trial_context_rule() {
    let Payload::FailureCarried(mut carried) = sample(EventKind::FailureCarried) else {
        unreachable!()
    };
    carried.failure = Failure {
        trial: Some(trial()),
        condition: None,
        ..carried.failure
    };
    let text = Payload::from(carried).message();
    assert!(text.contains("trial 0413"), "{text}");
    assert!(!text.contains("no trial was in flight"), "{text}");
}

#[test]
fn a_refused_preflight_check_says_it_failed() {
    let check = |ok| {
        Payload::from(PreflightCheck {
            check: "driver".into(),
            detail: "unsupported CPU".into(),
            ok,
        })
        .message()
    };
    assert_eq!(check(false), "preflight driver: FAILED (unsupported CPU)");
    assert_eq!(check(true), "preflight driver: ok (unsupported CPU)");
}

#[test]
fn profile_change_says_whether_the_profile_moved() {
    let change = |from: Option<&[i64]>, to: &[i64]| {
        Payload::from(ProfileChange {
            from: from.map(profile),
            to: profile(to),
        })
        .message()
    };
    let same = change(Some(&[-30, -20]), &[-30, -20]);
    assert!(same.starts_with("profile unchanged at"), "{same}");
    assert!(same.contains("-30") && same.contains("-20"), "{same}");
    let moved = change(Some(&[-30, -20]), &[-30, -25]);
    assert!(moved.starts_with("profile changed"), "{moved}");
    assert!(moved.contains("->") && moved.contains("-25"), "{moved}");
    let first = change(None, &[-30, -20]);
    assert!(first.starts_with("profile for checking"), "{first}");
}

#[test]
fn dead_end_names_whether_grub_is_cleared_and_whether_the_machine_reboots() {
    let dead_end = |action| {
        Payload::from(DeadEnd {
            condition: DeadEndCondition::FailureAtZero,
            core: None,
            detail: "failed at zero".into(),
            action,
        })
        .message()
    };
    assert_eq!(
        dead_end(Some(DeadEndAction::ClearSavedEntry)),
        "dead end failure_at_zero: failed at zero; clearing GRUB's saved entry"
    );
    assert_eq!(
        dead_end(Some(DeadEndAction::ClearSavedEntryAndReboot)),
        "dead end failure_at_zero: failed at zero; clearing GRUB's saved entry and rebooting"
    );
    assert_eq!(
        dead_end(Some(DeadEndAction::Exit)),
        "dead end failure_at_zero: failed at zero"
    );
}

#[test]
fn boot_saved_entry_never_claims_a_clear_that_failed() {
    let saved =
        |before: Option<&str>, after: Option<&str>, error: Option<&str>, reason: Option<&str>| {
            Payload::from(BootSavedEntry {
                before: before.map(Into::into),
                after: after.map(Into::into),
                error: error.map(Into::into),
                reason_error: reason.map(Into::into),
            })
            .message()
        };
    let failed = saved(Some("togi"), None, Some("grub-editenv failed"), None);
    assert!(failed.contains("not cleared"), "{failed}");
    assert!(failed.contains("grub-editenv failed"), "{failed}");
    assert!(!failed.contains("next boot selects"), "{failed}");
    assert_eq!(
        saved(None, None, None, None),
        "GRUB saved entry was already unset"
    );
    let cleared = saved(Some("togi"), None, None, Some("ESP is read-only"));
    assert!(cleared.contains("togi cleared"), "{cleared}");
    assert!(
        cleared.ends_with("leave reason not saved: ESP is read-only"),
        "{cleared}"
    );
}

fn round(event: RoundEvent, passed: bool, cores: &[u8], reason: Option<&str>) -> String {
    let Payload::DeepeningRound(mut round) = sample(EventKind::DeepeningRound) else {
        unreachable!()
    };
    round.event = event;
    round.passed = passed;
    round.cores = set(cores);
    round.reason = reason.map(Into::into);
    Payload::from(round).message()
}

#[test]
fn deepening_round_messages_say_who_moves_and_whether_the_round_passed() {
    let one = round(RoundEvent::Start, false, &[7], Some("solo limit check"));
    assert!(
        one.starts_with("deepening round 2 start: core 07 toward"),
        "{one}"
    );
    assert!(one.ends_with("; solo limit check"), "{one}");
    let many = round(RoundEvent::Start, false, &[0, 1, 2], None);
    assert!(
        many.starts_with("deepening round 2 start: 3 cores toward"),
        "{many}"
    );
    assert_eq!(
        round(RoundEvent::End, true, &[], Some("checks complete")),
        "deepening round 2 end: passed; checks complete"
    );
    let failed = round(RoundEvent::End, false, &[], Some("failure"));
    assert_eq!(failed, "deepening round 2 end: failure");
    assert_eq!(
        round(RoundEvent::End, false, &[], None),
        "deepening round 2 end: not passed"
    );
}

#[test]
fn hunt_start_says_where_the_failure_came_from_and_how_the_cores_are_parked() {
    let Payload::HuntStart(mut start) = sample(EventKind::HuntStart) else {
        unreachable!()
    };
    assert_eq!(
        Payload::from(start.clone()).message(),
        "hunt 3: unattributed failure in together R7 trial 0413; parked offsets from cycle end #811; candidates 00, 01; groups of 5 × 120 s; known failure"
    );
    start.reason = None;
    start.parked_seq = None;
    start.parked = profile(&[-30, -28, 0, 0]);
    start.cores = set(&[0, 1]);
    start.candidates = set(&[2, 3]);
    assert_eq!(
        Payload::from(start.clone()).message(),
        "hunt 3: unattributed failure in together R7 trial 0413; loaded cores 00, 01 held at their failing offsets, the rest parked at 0; candidates 02, 03; groups of 5 × 120 s"
    );
    start.parked = profile(&[0, 0, 0, 0]);
    start.trial = None;
    assert_eq!(
        Payload::from(start).message(),
        "hunt 3: unattributed failure in idle; parked at 0; candidates 02, 03; groups of 5 × 120 s"
    );
}

#[test]
fn hunt_group_lists_its_cores_comma_separated() {
    let Payload::HuntGroup(mut group) = sample(EventKind::HuntGroup) else {
        unreachable!()
    };
    group.stage = HuntStage::Part;
    group.group = ord(4);
    group.cores = set(&[8, 9, 10, 11]);
    group.probe = None;
    group.held.clear();
    group.inferred = None;
    group.reason = None;
    assert_eq!(
        Payload::from(group.clone()).message(),
        "hunt 3 group 4: cores 08, 09, 10, 11 at failing offsets, the rest parked; trials of 120 s"
    );
    group.group = ord(9);
    group.probe = Some(member(11, -22));
    group.held = vec![member(3, -40)];
    assert_eq!(
        Payload::from(group.clone()).message(),
        "hunt 3 group 9: core 11 at -22 with core 03 -40, the rest parked; trials of 120 s"
    );
    group.inferred = Some(Inference::Pass);
    group.reason = Some("complement passed".into());
    assert!(
        Payload::from(group)
            .message()
            .ends_with("pass inferred: complement passed")
    );
}

#[test]
fn carry_without_failure_points_says_they_stay_behind_and_why() {
    let Payload::SessionCarried(mut carried) = sample(EventKind::SessionCarried) else {
        unreachable!()
    };
    carried.failure_points = false;
    carried.detail = Some("BIOS context changed".into());
    let text = Payload::from(carried.clone()).message();
    assert!(
        text.contains("carried 1 candidate solo limit from"),
        "{text}"
    );
    assert!(
        text.contains("failure points stay behind: BIOS context changed"),
        "{text}"
    );
    assert!(!text.contains("failure point from"), "{text}");
    carried.detail = None;
    assert!(
        Payload::from(carried)
            .message()
            .ends_with("no reason recorded")
    );
}

#[test]
fn applied_profiles_name_the_condition_they_apply_to() {
    let applied = |condition| {
        Payload::from(ProfileApplied {
            offsets: profile(&[-30, 0]),
            condition,
        })
        .message()
    };
    assert!(applied(Condition::Parked).starts_with("group profile applied"));
    assert!(applied(Condition::Together).starts_with("profile applied"));
    let alone = applied(Condition::Alone);
    assert!(alone.contains("alone") && alone.contains("CO 0"), "{alone}");
    assert!(!alone.contains("-30"), "{alone}");
}

#[test]
fn carried_trials_keep_class_outcome_duration_profile_and_requesters() {
    let Payload::TrialCarried(mut carried) = sample(EventKind::TrialCarried) else {
        unreachable!()
    };
    let pass = Payload::from(carried.clone()).message();
    for part in [
        "carried pass",
        "passed",
        "top requester cores 03",
        "after 90 s",
        "intended 90 s",
        "-30",
        "alone R2",
    ] {
        assert!(pass.contains(part), "{part:?} missing from {pass}");
    }
    carried.outcome = Outcome::Failure;
    carried.signal = Some(Signal::Crash);
    carried.duration_s = 7;
    carried.top_requesters = CoreSet::default();
    let text = Payload::from(carried.clone()).message();
    for part in [
        "carried failure",
        "failed (crash)",
        "after 7 s",
        "intended 90 s",
    ] {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
    assert!(!text.contains("top requester"), "{text}");
    carried.outcome = Outcome::Inconclusive;
    assert!(
        Payload::from(carried)
            .message()
            .contains("was inconclusive")
    );
}

#[test]
fn carried_trials_name_their_class() {
    let Payload::TrialCarried(mut carried) = sample(EventKind::TrialCarried) else {
        unreachable!()
    };
    let text = Payload::from(carried.clone()).message();
    assert!(
        text.contains("alone R2 mprime AVX2 36K-248K on cores 03 passed"),
        "{text}"
    );
    carried.condition = Condition::Together;
    carried.class.regime = Regime::R7;
    carried.class.workload = None;
    carried.class.cores = set(&[1, 5]);
    let text = Payload::from(carried).message();
    assert!(
        text.contains("together R7 any workload on cores 01, 05 passed"),
        "{text}"
    );
    assert!(!text.contains("AVX2"), "{text}");
}

#[test]
fn a_raw_reset_line_is_kept_verbatim_over_the_typed_reason() {
    let Payload::CrashDetected(mut crash) = sample(EventKind::CrashDetected) else {
        unreachable!()
    };
    let raw = "WDT: hardware watchdog expired (SP5100 TCO)";
    crash.reset_reason_raw = vec![raw.into()];
    let text = Payload::from(crash).message();
    assert!(text.contains(&format!("reset reason: {raw}")), "{text}");
    assert!(!text.contains("reset reason: watchdog"), "{text}");
}

#[test_case(Outcome::Pass; "pass")]
#[test_case(Outcome::Failure; "failure")]
#[test_case(Outcome::Inconclusive; "inconclusive")]
fn trial_end_voltage_telemetry_survives_every_outcome(outcome: Outcome) {
    let Payload::TrialEnd(base) = sample(EventKind::TrialEnd) else {
        unreachable!()
    };
    let end = TrialEnd {
        outcome,
        reason: Some("interrupted".into()),
        ..base
    };
    let text = Payload::from(end).message();
    assert!(
        text.contains("loaded voltage request median 1.100 V, min 1.050 V"),
        "{outcome}: {text}"
    );
    assert!(
        text.contains("top requester 03 1.125 V"),
        "{outcome}: {text}"
    );
}

#[test]
fn tied_top_requesters_are_all_listed() {
    let Payload::TrialEnd(mut end) = sample(EventKind::TrialEnd) else {
        unreachable!()
    };
    end.top_requesters = set(&[3, 5]);
    let text = Payload::from(end).message();
    assert!(
        text.contains("top requester 03 1.125 V, 05 1.100 V"),
        "{text}"
    );
}

#[test]
fn a_between_trials_mce_is_recorded_only() {
    let Payload::Mce(mut mce) = sample(EventKind::Mce) else {
        unreachable!()
    };
    mce.trial = None;
    mce.between_trials = true;
    let text = Payload::from(mce.clone()).message();
    assert!(text.ends_with("between trials (recorded only)"), "{text}");
    mce.between_trials = false;
    assert!(!Payload::from(mce).message().contains("between trials"));
}

#[test]
fn a_typed_reset_reason_is_named_when_no_raw_line_exists() {
    let Payload::CrashDetected(mut crash) = sample(EventKind::CrashDetected) else {
        unreachable!()
    };
    crash.reset_reason_raw.clear();
    let text = Payload::from(crash.clone()).message();
    assert!(text.contains("reset reason: watchdog"), "{text}");
    assert!(
        text.contains("inconclusive: reset does not establish a tuning failure"),
        "{text}"
    );
    crash.inconclusive = false;
    assert!(!Payload::from(crash).message().contains("inconclusive"));
}

#[test]
fn a_deepen_decision_says_deepened_not_failed() {
    let Payload::TunerDecision(mut decision) = sample(EventKind::TunerDecision) else {
        unreachable!()
    };
    decision.activity = Activity::Deepening;
    decision.decision = Decision::Deepen;
    decision.from_offset = target(-30);
    decision.to_offset = target(-31);
    decision.reason = "checks passed".into();
    assert_eq!(
        Payload::from(decision).message(),
        "core 07 deepened from -30 to -31 (checks passed)"
    );
}

fn decision_text(kind: Decision, activity: Activity, from: i64, to: i64) -> String {
    let Payload::TunerDecision(mut decision) = sample(EventKind::TunerDecision) else {
        unreachable!()
    };
    decision.activity = activity;
    decision.decision = kind;
    decision.from_offset = target(from);
    decision.to_offset = target(to);
    decision.reason = "why".into();
    Payload::from(decision).message()
}

#[test]
fn each_decision_names_its_own_source_and_destination() {
    use Activity::{Checking, Deepening, Search};
    assert_eq!(
        decision_text(Decision::StepDeeper, Search, -10, -15),
        "core 07 passed R1+R2 at -10; searches -15 on its next turn (why)"
    );
    assert_eq!(
        decision_text(Decision::Backoff, Search, -10, -6),
        "core 07 failed at -10; searches -6 on its next turn (why)"
    );
    assert_eq!(
        decision_text(Decision::CheckSoloLimit, Checking, -7, -6),
        "core 07 checks candidate solo limit -6 (from -7; why)"
    );
    assert_eq!(
        decision_text(Decision::CheckSoloLimit, Checking, -6, -6),
        "core 07 checks candidate solo limit -6 (why)"
    );
    assert_eq!(
        decision_text(Decision::Deepen, Deepening, -10, -11),
        "core 07 deepened from -10 to -11 (why)"
    );
    assert_eq!(
        decision_text(Decision::Yield, Deepening, -6, -5),
        "core 07 yielded from -6 to -5 (why)"
    );
    assert_eq!(
        decision_text(Decision::Backoff, Checking, -25, -24),
        "core 07 backed off from -25 to -24 (why)"
    );
    assert_eq!(
        decision_text(Decision::Hold, Checking, -24, -24),
        "core 07 failure held; tuning stays at -24 (why)"
    );
}

#[test]
fn a_checking_step_does_not_claim_a_regime() {
    let text = Payload::from(CheckingStep {
        cycle: ord(3),
        step: ord(1),
        profile: profile(&[-10]),
    })
    .message();
    assert!(text.starts_with("checking cycle 3 step 1 starts"), "{text}");
    assert!(!text.contains("R7") && !text.contains("R2"), "{text}");
}

#[test]
fn a_checking_chain_without_request_telemetry_says_it_fell_back_to_offsets() {
    let Payload::CheckingChain(mut chain) = sample(EventKind::CheckingChain) else {
        unreachable!()
    };
    chain.source_seqs.clear();
    let text = Payload::from(chain.clone()).message();
    assert!(
        text.contains("offset fallback (no request telemetry)"),
        "{text}"
    );
    assert!(text.contains("partial 2 loads cores 03, 04"), "{text}");
    let measured = Payload::from(CheckingChain {
        source_seqs: vec![seq(5), seq(6)],
        ..chain
    })
    .message();
    assert!(
        measured.contains("request measurements #5, #6"),
        "{measured}"
    );
    assert!(!measured.contains("fallback"), "{measured}");
}

#[test]
fn defect_messages_name_the_diagnosis_and_the_answer() {
    let Payload::DefectFound(mut found) = sample(EventKind::DefectFound) else {
        unreachable!()
    };
    let text = Payload::from(found.clone()).message();
    for part in [
        "defect 1",
        "pull request #14",
        "too_cautious",
        "decisions #20",
        "affected cores 02",
        "clean exit",
    ] {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
    found.detail.clear();
    assert!(
        Payload::from(found)
            .message()
            .ends_with("affected cores 02")
    );
    assert_eq!(
        message(EventKind::DefectAnswered),
        "defect 1: answered yes to reset cores 02"
    );
}

#[test]
fn a_previous_boot_leave_reason_carries_the_restart_count() {
    assert_eq!(
        message(EventKind::BootLeaveReason),
        "previous tuning boot ended: restart limit (consecutive restart-limit boots: 3)"
    );
}

#[test]
fn an_unavailable_ranking_gives_the_cause_and_the_fallback_order() {
    let Payload::HostRanking(mut ranking) = sample(EventKind::HostRanking) else {
        unreachable!()
    };
    let ok = Payload::from(ranking.clone()).message();
    assert!(ok.starts_with("preferred cores 03 01"), "{ok}");
    ranking.detail = Some("firmware unavailable".into());
    let text = Payload::from(ranking).message();
    assert!(
        text.contains("unavailable (firmware unavailable)"),
        "{text}"
    );
    assert!(text.ends_with("core-id order"), "{text}");
}

#[test]
fn monotonicity_warnings_use_the_detail_or_the_trial_and_pass_count() {
    let Payload::TunerWarning(mut warning) = sample(EventKind::TunerWarning) else {
        unreachable!()
    };
    assert_eq!(
        Payload::from(warning.clone()).message(),
        "monotonicity: failed at -30"
    );
    warning.detail.clear();
    let text = Payload::from(warning.clone()).message();
    assert!(
        text.contains("trial 0413 failed on a profile at least as shallow as 2 passes"),
        "{text}"
    );
    warning.trial = None;
    let text = Payload::from(warning).message();
    assert!(
        text.contains("idle failure") && text.contains("2 passes"),
        "{text}"
    );
}

#[test]
fn an_exposure_names_its_ccd_position_offset_and_evidence() {
    assert_eq!(
        message(EventKind::ExposureObserved),
        "CCD0 R8 light core 03 at -20: 120 s eligible, no failure"
    );
}

#[test]
fn night_activities_are_named_in_the_trial_intent_message() {
    let text = message(EventKind::TrialIntent);
    assert!(text.contains("mapping task mapping #4"), "{text}");
    assert!(text.contains("testing CCD0 at -32"), "{text}");
}

#[test]
fn a_failed_requalification_names_its_night() {
    let Payload::DayVeto(mut veto) = sample(EventKind::DayVeto) else {
        unreachable!()
    };
    veto.event = DayEventId::Requalification {
        night: NightId(Seq::new(41).unwrap()),
    };
    let text = Payload::from(veto).message();
    assert!(text.contains("re-qualification of night #41"), "{text}");
}

/// The sample `day.step` with `step` as its kind, the profile from CCD0 -10, CCD1 -10 to `proposed`
/// and no reason, so the text is the decision's own.
fn day_step_message(step: StepKind, proposed: &[(i64, i64)]) -> String {
    let Payload::DayStep(mut sample) = sample(EventKind::DayStep) else {
        unreachable!()
    };
    sample.step = step;
    sample.proposed.ccd = ccd_profile(proposed);
    sample.reason = String::new();
    Payload::from(sample).message()
}

fn ccd_profile(offsets: &[(i64, i64)]) -> togi_machine::CcdProfile {
    togi_machine::CcdProfile::try_from(
        offsets
            .iter()
            .map(|&(ccd, at)| (togi_machine::CcdId::new(ccd).unwrap(), target(at)))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap()
}

fn requalification(night: u64) -> DayEventId {
    DayEventId::Requalification {
        night: NightId(Seq::new(night).unwrap()),
    }
}

#[test_case(
    RetreatCause::ShallowerCap {},
    "a cap became shallower"
; "cap")]
#[test_case(
    RetreatCause::Event {
        event: requalification(41),
        escalation: Escalation::RevertNewest { reverted: DecisionId(2) },
    },
    "re-qualification of night #41, reverting the newest step (decision 2)"
; "event reverting the newest step")]
#[test_case(
    RetreatCause::Event {
        event: requalification(41),
        escalation: Escalation::EveryCcd { strike: 2 },
    },
    "re-qualification of night #41, every CCD one step back (strike 2)"
; "event stepping every CCD back")]
#[test_case(
    RetreatCause::Completion {},
    "a held retreat is now qualified"
; "completion")]
fn a_retreat_message_names_the_ccds_the_profiles_and_the_cause(cause: RetreatCause, because: &str) {
    let text = day_step_message(
        StepKind::Retreat {
            changed: vec![togi_machine::CcdId::new(0).unwrap()],
            cause,
            wanted: None,
        },
        &[(0, -5), (1, -10)],
    );
    assert_eq!(
        text,
        format!(
            "decision 3: retreat CCD0 from CCD0 -10, CCD1 -10 to CCD0 -5, CCD1 -10 because {because} \
             (proposal; not applied)"
        )
    );
}

#[test]
fn a_retreat_that_fell_back_to_co_0_names_the_profile_it_waits_for() {
    let Payload::DayStep(sample) = sample(EventKind::DayStep) else {
        unreachable!()
    };
    let wanted = CcdVector {
        ccd: ccd_profile(&[(0, -15), (1, -10)]),
        cores: sample.old.cores,
    };
    let text = day_step_message(
        StepKind::Retreat {
            changed: [0, 1]
                .map(|ccd| togi_machine::CcdId::new(ccd).unwrap())
                .to_vec(),
            cause: RetreatCause::ShallowerCap {},
            wanted: Some(wanted),
        },
        &[(0, 0), (1, 0)],
    );
    assert_eq!(
        text,
        "decision 3: retreat CCD0, CCD1 from CCD0 -10, CCD1 -10 to CCD0 0, CCD1 0 because a cap \
         became shallower; CO 0 holds until CCD0 -15, CCD1 -10 is qualified (proposal; not applied)"
    );
}

#[test]
fn a_firmware_zero_message_names_the_reset_ccds_and_profiles() {
    let text = day_step_message(
        StepKind::FirmwareZero {
            changed: [0, 1]
                .map(|ccd| togi_machine::CcdId::new(ccd).unwrap())
                .to_vec(),
        },
        &[(0, 0), (1, 0)],
    );
    assert_eq!(
        text,
        "decision 3: firmware changed: set CCD0, CCD1 from CCD0 -10, CCD1 -10 to CCD0 0, CCD1 0 \
         (proposal; not applied)"
    );
}
