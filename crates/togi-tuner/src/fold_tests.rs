use std::num::NonZeroU32;

use test_case::test_case;
use togi_journal::{
    Activity, Attribution, CorePhase, CoreTransition, DeadEnd, DeadEndCondition, Decision, Failure,
    Mce, Notice, Outcome, Payload, Seq, TrialId, TrialProgress, TunerDecision,
};
use togi_machine::{BankType, Condition, CoreSet, CpuId, PerCore, Regime, Signal, workloads};

use crate::cores::Rotation;
use crate::schedule_tests::transition;
use crate::testkit::{Journal, Start, set};
use crate::tests::{core, off};
use crate::{
    Action, DecisionAction, DecisionPayload, FoldError, Load, PartNo, StepNo, TrialContext,
    TrialPlan, WorkloadChoice,
};

const SESSION: &str = "20261002T004254Z";

fn failure(condition: Option<Condition>, trial: Option<&TrialId>) -> Failure {
    Failure {
        signal: Signal::Crash,
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        trial: trial.cloned(),
        regime: None,
        condition,
        profile: None,
        known_failure: None,
        reason: None,
        round: None,
    }
}

fn known(core_id: u8, offset: i64, seq: Seq) -> Failure {
    Failure {
        attribution: Attribution::Attributed,
        core: Some(core(core_id)),
        offset: Some(off(offset)),
        known_failure: Some(seq),
        condition: Some(Condition::Alone),
        ..failure(None, None)
    }
}

fn mce(trial: Option<&TrialId>, core_id: Option<u8>, bank_type: BankType) -> Mce {
    Mce {
        cpu: CpuId::from(0),
        core: core_id.map(core),
        bank: 0,
        bank_type,
        corrected: false,
        monotonic_ns: None,
        from_boot: None,
        trial: trial.cloned(),
        between_trials: false,
        lines: Vec::new(),
    }
}

/// Fails a together trial of `regime` at `profile` with a crash, naming `end` as the end's core.
fn fail_together(j: &mut Journal, regime: Regime, profile: &[i64], end: Option<u8>) -> TrialId {
    let (cores, duration) = if regime == Regime::R7 {
        (&[0, 8][..], 120)
    } else {
        (&[0, 1][..], 900)
    };
    let plan = j.manual(
        regime,
        0,
        Load::Many(set(cores)),
        Condition::Together,
        profile,
        duration,
    );
    let (intent, trial) = j.intent(&plan);
    let mut payload = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::Crash));
    payload.core = end.map(core);
    j.append(payload.into(), &[intent]);
    trial
}

fn attributes(j: &Journal, trial: &TrialId) -> bool {
    matches!(
        j.state.next(),
        Ok(Action::Decide(DecisionAction { payload: DecisionPayload::Failure(failure), .. }))
            if failure.trial.as_ref() == Some(trial)
    )
}

#[test]
fn a_together_failure_awaiting_attribution_is_attributed_and_folds() {
    let mut j = Journal::new(&[0, 1], &[Start::at_limit(0), Start::at_limit(-12)]);
    let trial = fail_together(&mut j, Regime::R6, &[0, -12], None);
    let action = j.next_decision();
    let DecisionPayload::Failure(attributed) = &action.payload else {
        panic!("expected a failure, got {action:?}");
    };
    assert_eq!(attributed.attribution, Attribution::Attributed);
    assert_eq!(attributed.core, Some(core(1)));
    assert_eq!(attributed.offset, Some(off(-12)));
    assert_eq!(attributed.trial.as_ref(), Some(&trial));
    let seq = j.decide(action);
    assert_eq!(j.run().core(core(1)).pending, Some(seq));
    assert!(!attributes(&j, &trial));
    j.assert_resumes();
}

#[test]
fn a_multi_core_r7_failure_is_recorded_and_left_to_the_r7_rules() {
    let mut j = Journal::new(&[0, 8], &[Start::at_limit(-5), Start::at_limit(-5)]);
    fail_together(&mut j, Regime::R7, &[-5, -5], Some(8));
    let action = j.next_decision();
    let seq = j.decide(action);
    let run = j.run();
    assert!(run.failures.pending(seq).is_some());
    assert_eq!(run.core(core(8)).pending, None);
    assert!(run.failures.queue_is_empty());
}

#[test]
fn a_known_multi_core_r7_failure_leaves_the_cores_alone() {
    let mut j = Journal::has_room(&[-10, -10]);
    let fact = j.carry(
        SESSION,
        (Regime::R7, 0, &[0, 1], 120),
        Condition::Together,
        &[-10, -10],
        Outcome::Failure,
    );
    let seq = j.failure(
        Failure {
            attribution: Attribution::Unattributed,
            core: None,
            ..known(0, -10, fact)
        },
        &[fact],
    );
    assert!(j.run().failures.pending(fact).is_some());
    assert_eq!(j.run().core(core(0)).pending, None);
    let _ = seq;
}

#[test]
fn a_failure_with_no_trial_that_is_not_an_idle_crash_has_no_class() {
    let mut j = Journal::has_room(&[-10]);
    assert!(matches!(
        j.try_append(failure(None, None).into(), &[]),
        Err(FoldError::NoFailureClass)
    ));
}

#[test]
fn an_idle_failure_without_a_profile_is_charged_to_the_checking_profile() {
    let mut j = Journal::has_room(&[-10, -20]);
    let first = j.failure(failure(Some(Condition::Together), None), &[]);
    assert_eq!(
        j.run().failures.pending(first).unwrap().fact.profile,
        j.profile(&[-10, -20])
    );
    j.profile_change(&[-9, -19]);
    let second = j.failure(failure(Some(Condition::Together), None), &[]);
    assert_eq!(
        j.run().failures.pending(second).unwrap().fact.profile,
        j.profile(&[-9, -19])
    );
}

#[test]
fn a_newer_failure_of_a_core_retires_the_one_it_replaces() {
    let mut j = Journal::search_at(&[-30]);
    let a = j.carry(
        SESSION,
        (Regime::R1, 0, &[0], 90),
        Condition::Alone,
        &[-29],
        Outcome::Failure,
    );
    let b = j.carry(
        SESSION,
        (Regime::R1, 0, &[0], 90),
        Condition::Alone,
        &[-28],
        Outcome::Failure,
    );
    j.failure(known(0, -29, a), &[a]);
    assert_eq!(j.run().core(core(0)).pending, Some(a));
    j.failure(known(0, -28, b), &[b]);
    assert!(j.run().failures.pending(a).is_none());
    assert!(j.run().failures.pending(b).is_some());
    assert_eq!(j.run().core(core(0)).pending, Some(b));
}

#[test]
fn a_pending_failure_of_a_core_not_in_search_is_backed_off() {
    let mut j = Journal::has_room(&[-10]);
    let seq = j.failure(
        Failure {
            attribution: Attribution::Attributed,
            core: Some(core(0)),
            offset: Some(off(-10)),
            ..failure(Some(Condition::Together), None)
        },
        &[],
    );
    assert_eq!(j.run().core(core(0)).pending, Some(seq));
    let Some(crate::Action::Decide(action)) = j.run().pending_core_action() else {
        panic!("the pending failure was not answered");
    };
    assert!(matches!(
        action.payload,
        crate::DecisionPayload::TunerDecision(_)
    ));
    assert_eq!(action.cause, [seq]);
}

#[test]
fn phase_decision_names_the_limit_a_core_reaches() {
    let j = Journal::new(&[0], &[Start::has_room(-10).fail(-11)]);
    let action = j.run().phase_decision().unwrap();
    let DecisionPayload::CorePhase(phase) = action.payload else {
        panic!("expected core.phase");
    };
    assert_eq!(
        (phase.from, phase.to),
        (Some(CorePhase::HasRoom), CorePhase::AtLimit)
    );
    assert!(
        phase.reason.contains("failure point -11"),
        "{}",
        phase.reason
    );
}

#[test]
fn nothing_is_decided_before_every_core_has_a_phase_or_while_one_searches() {
    let j = Journal::new(&[0, 1], &[Start::has_room(-10)]);
    assert_eq!(j.state.next().ok(), None);
    let j = Journal::search_at(&[-10]);
    assert!(j.run().profile_decision().is_none());
    let j = Journal::has_room(&[-10]);
    assert!(j.run().profile_decision().is_some());
}

#[test]
fn a_candidate_check_needs_its_r1_and_r2_workloads() {
    let bad: [Vec<_>; 3] = [
        Vec::new(),
        vec![workloads(Regime::R2)[0].id(), workloads(Regime::R1)[0].id()],
        vec![workloads(Regime::R1)[0].id()],
    ];
    for workloads in bad {
        let mut j = Journal::search_at(&[-20]);
        let transition = CoreTransition {
            check_solo_limit: true,
            workloads: workloads.clone(),
            ..transition(0, CorePhase::Search, -20)
        };
        assert!(
            matches!(
                j.try_append(transition.into(), &[]),
                Err(FoldError::BadCandidate(found)) if found == core(0)
            ),
            "{workloads:?}"
        );
        let mut j = Journal::search_at(&[-20]);
        let decision = TunerDecision {
            core: core(0),
            activity: Activity::Search,
            decision: Decision::CheckSoloLimit,
            from_offset: off(-20),
            to_offset: off(-20),
            pass: None,
            failure_point: None,
            workloads,
            reason: "test".to_owned(),
        };
        assert!(matches!(
            j.try_append(decision.into(), &[]),
            Err(FoldError::BadCandidate(found)) if found == core(0)
        ));
    }
}

#[test]
fn a_decision_recording_a_zero_failure_point_stops_the_session() {
    let mut j = Journal::search_at(&[-5]);
    let seq = j.decision(TunerDecision {
        core: core(0),
        activity: Activity::Search,
        decision: Decision::Backoff,
        from_offset: off(-5),
        to_offset: off(0),
        pass: None,
        failure_point: Some(off(0)),
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    assert_eq!(j.run().core(core(0)).zero_seq, Some(seq));
    let stop = j.next_decision();
    assert!(matches!(stop.payload, DecisionPayload::DeadEnd(_)));
    assert_eq!(stop.cause, [seq]);
}

#[test]
fn rotation_counts_each_regime_apart_and_all_loads_apart_from_one_core() {
    let mut rotation = Rotation::default();
    for (n, regime) in togi_machine::REGIMES.into_iter().enumerate() {
        for _ in 0..=n {
            rotation.advance(regime);
        }
    }
    for (n, regime) in togi_machine::REGIMES.into_iter().enumerate() {
        assert_eq!(rotation.get(regime), n + 1);
    }
    let mut j = Journal::has_room(&[-10, -10]);
    let checking = j.run().config.checking_trial.get();
    let plan = |j: &Journal, regime, load, duration| {
        j.manual(regime, 0, load, Condition::Together, &[-10, -10], duration)
    };
    let all = plan(&j, Regime::R6, Load::Many(set(&[0, 1])), 900);
    let one = plan(&j, Regime::R1, Load::Single(core(1)), checking);
    j.trial(&all, Outcome::Pass);
    j.trial(&one, Outcome::Pass);
    let r6_one = plan(&j, Regime::R6, Load::Single(core(1)), 900);
    let r1_all = plan(&j, Regime::R1, Load::Many(set(&[0, 1])), checking);
    let other = plan(&j, Regime::R1, Load::Single(core(0)), checking);
    assert_eq!(
        (
            j.rotation(&all),
            j.rotation(&r6_one),
            j.rotation(&one),
            j.rotation(&r1_all),
            j.rotation(&other)
        ),
        (1, 0, 1, 0, 0)
    );
}

fn trial_intent(j: &Journal) -> (togi_journal::TrialIntent, TrialPlan) {
    let plan = j.next_trial();
    (plan.intent(TrialId::from("0001".to_owned()), 0), plan)
}

#[test_case(Some(0), &[], Load::Single(core(0)); "single core")]
#[test_case(None, &[0, 1], Load::Many(set(&[0, 1])); "multiple cores")]
#[test_case(None, &[1], Load::Many(set(&[1])); "one core in a set")]
#[test_case(Some(0), &[9], Load::Single(core(0)); "named core has precedence")]
#[test_case(Some(0), &[0, 1], Load::Single(core(0)); "named core ignores nonempty set")]
fn intent_load_decoding_preserves_the_recorded_shape(
    single: Option<u8>,
    many: &[u8],
    expected: Load,
) {
    let j = Journal::search_at(&[-10, -10]);
    let (mut intent, mut plan) = trial_intent(&j);
    intent.core = single.map(core);
    intent.cores = set(many);
    assert_eq!(Load::recorded(intent.core, intent.cores), expected);
    assert_eq!(
        Load::decode(&intent, &crate::tests::topology(&[0, 1])).unwrap(),
        expected
    );
    plan.load = expected;
    let canonical = plan.intent(intent.trial, 0);
    assert!(canonical.core.is_none() || canonical.cores.is_empty());
    assert_eq!(Load::recorded(canonical.core, canonical.cores), expected);
}

#[test_case(None, &[], None; "empty load")]
#[test_case(Some(9), &[], Some(9); "foreign single core")]
#[test_case(None, &[0, 9], Some(9); "foreign core in a set")]
#[test_case(Some(9), &[0], Some(9); "foreign named core does not fall back to set")]
fn intent_load_decoding_validates_single_and_multi_core_loads(
    single: Option<u8>,
    many: &[u8],
    unknown: Option<u8>,
) {
    let mut j = Journal::search_at(&[-10, -10]);
    let (mut intent, _) = trial_intent(&j);
    intent.core = single.map(core);
    intent.cores = set(many);
    let error = j.try_append(intent.into(), &[]).unwrap_err();
    match unknown {
        Some(id) => assert!(matches!(error, FoldError::UnknownCore(found) if found == core(id))),
        None => assert!(
            matches!(error, FoldError::NoLoad(trial) if trial == TrialId::from("0001".to_owned()))
        ),
    }
}

#[test]
fn an_intent_naming_another_regimes_workload_a_missing_load_or_a_foreign_core_is_refused() {
    type Change = fn(&mut togi_journal::TrialIntent);
    type Expected = fn(&FoldError) -> bool;
    let cases: [(&str, Change, Expected); 4] = [
        (
            "workload of another regime",
            |intent| intent.workload = workloads(Regime::R2)[0].id(),
            |error| {
                matches!(error, FoldError::WorkloadRegime { regime: Regime::R1, workload }
                    if *workload == workloads(Regime::R2)[0].id())
            },
        ),
        (
            "no loaded core",
            |intent| intent.core = None,
            |error| matches!(error, FoldError::NoLoad(trial) if *trial == TrialId::from("0001".to_owned())),
        ),
        (
            "loaded cores outside the topology",
            |intent| {
                intent.core = None;
                intent.cores = set(&[9]);
            },
            |error| matches!(error, FoldError::UnknownCore(found) if *found == core(9)),
        ),
        (
            "core outside the topology",
            |intent| intent.core = Some(core(9)),
            |error| matches!(error, FoldError::UnknownCore(found) if *found == core(9)),
        ),
    ];
    for (name, change, expected) in cases {
        let mut j = Journal::search_at(&[-10]);
        let (mut intent, _) = trial_intent(&j);
        change(&mut intent);
        assert!(
            matches!(j.try_append(intent.into(), &[]), Err(error) if expected(&error)),
            "{name}"
        );
    }
}

#[test]
fn a_recorded_configuration_this_build_refuses_is_not_folded() {
    let mut j = Journal::search_at(&[-10]);
    let mut snapshot = togi_config::Config::default().snapshot();
    snapshot.durations.search_trial_s = 0;
    let loaded = togi_journal::ConfigLoaded {
        build: crate::testkit::build(),
        config: snapshot,
        path: "/etc/togi/config.json".to_owned(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    };
    let error = j.try_append(loaded.into(), &[]).unwrap_err();
    assert!(matches!(error, FoldError::Config(_)));
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<std::sync::Arc<togi_config::ConfigError>>())
        .expect("config.loaded refusal keeps the config error as its source");
    assert!(matches!(
        &**source,
        togi_config::ConfigError::Duration {
            field: "search_trial_s",
            ..
        }
    ));
    assert_eq!(
        togi_machine::error_chain(&error),
        "config.loaded records a configuration this build refuses: durations.search_trial_s: 0 is not within [1, 86400]"
    );
}

#[test_case(0; "zero")]
#[test_case(65_536; "one beyond u16")]
#[test_case(u32::MAX; "largest journal count")]
fn out_of_range_hunt_trial_counts_are_refused(trials: u32) {
    let mut j = Journal::has_room(&[-10]);
    let start = numeric_hunt_start(&j, trials);
    let error = j.try_append(start.into(), &[]).unwrap_err();
    if trials == 0 {
        assert!(matches!(error, FoldError::ZeroTrials));
    } else {
        assert!(matches!(error, FoldError::HuntTrials(_)));
        assert!(std::error::Error::source(&error).is_some());
    }
}

fn numeric_hunt_start(j: &Journal, trials: u32) -> togi_journal::HuntStart {
    togi_journal::HuntStart {
        hunt: NonZeroU32::MIN,
        failure: Seq::new(1).unwrap(),
        trial: None,
        regime: Regime::R6,
        workload: workloads(Regime::R6)[0].id(),
        cores: set(&[0]),
        duration_s: 120,
        failing: j.profile(&[-10]).offsets(),
        parked: j.profile(&[0]).offsets(),
        parked_seq: None,
        candidates: set(&[0]),
        trials,
        trial_s: 120,
        miss: 0.0,
        rate: 0.0,
        ranking: vec![core(0)],
        reason: None,
    }
}

#[test_case(Some(0); "zero")]
#[test_case(Some(65_536); "one beyond u16")]
#[test_case(Some(u32::MAX); "largest journal count")]
fn out_of_range_round_trial_counts_are_refused(trials: Option<u32>) {
    let mut j = Journal::has_room(&[-10]);
    let round = numeric_round(&j, trials, Some(120));
    let error = j.try_append(round.into(), &[]).unwrap_err();
    if trials == Some(0) {
        assert!(matches!(error, FoldError::ZeroTrials));
    } else {
        assert!(matches!(error, FoldError::RoundTrials(_)));
        assert!(std::error::Error::source(&error).is_some());
    }
}

#[test_case(None; "absent count uses config")]
#[test_case(Some(1); "minimum count")]
#[test_case(Some(65_535); "maximum count")]
fn representable_round_trial_counts_remain_exact(trials: Option<u32>) {
    let mut j = Journal::has_room(&[-10]);
    let round = numeric_round(&j, trials, None);
    j.try_append(round.into(), &[]).unwrap();
    let view = j.state.project().deepening.unwrap();
    assert_eq!(view.checks[0].needed, trials.unwrap_or(5));
}

#[test]
fn a_zero_recorded_round_duration_is_refused() {
    let mut j = Journal::has_room(&[-10]);
    let round = numeric_round(&j, Some(5), Some(0));
    assert!(matches!(
        j.try_append(round.into(), &[]),
        Err(FoldError::ZeroDuration)
    ));
}

fn numeric_round(
    j: &Journal,
    trials: Option<u32>,
    trial_s: Option<u32>,
) -> togi_journal::DeepeningRound {
    togi_journal::DeepeningRound {
        round: NonZeroU32::MIN,
        event: togi_journal::RoundEvent::Start,
        base: j.profile(&[-10]).offsets(),
        base_seq: None,
        target: j.profile(&[-11]).offsets(),
        profile: j.profile(&[-11]).offsets(),
        cores: set(&[0]),
        ranking: vec![core(0)],
        trials,
        trial_s,
        passed: false,
        reason: None,
    }
}

#[test]
fn a_plan_records_its_context_in_its_intent() {
    let j = Journal::has_room(&[-10, -10]);
    let base = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-10, -10],
        120,
    );
    let many = base.intent(TrialId::from("1".to_owned()), 0);
    assert_eq!(
        (many.core, many.cores, many.offset),
        (None, set(&[0, 1]), None)
    );
    let nz = |n| NonZeroU32::new(n).unwrap();
    let with = |context| {
        let mut plan = base.clone();
        plan.requirement.context = context;
        plan.intent(TrialId::from("1".to_owned()), 0)
    };
    let cycle = with(TrialContext::Cycle {
        cycle: nz(3),
        step: StepNo::new(nz(2)),
        part: PartNo::new(nz(4)),
    });
    assert_eq!((cycle.cycle, cycle.step), (Some(nz(3)), Some(nz(2))));
    assert_eq!(StepNo::new(nz(2)).get(), nz(2));
    assert_eq!(PartNo::new(nz(4)).get(), nz(4));
    let seq = Seq::FIRST;
    assert!(with(TrialContext::Rerun { failure: seq }).rerun);
    assert!(with(TrialContext::ZeroRerun { failure: seq }).rerun);
    assert_eq!(
        with(TrialContext::Round { round: nz(2) }).round,
        Some(nz(2))
    );
    let hunt = with(TrialContext::Hunt {
        hunt: nz(5),
        group: nz(6),
    });
    assert_eq!((hunt.hunt, hunt.group), (Some(nz(5)), Some(nz(6))));
}

#[test]
fn mces_outside_the_open_trials_core_local_window_are_not_remembered() {
    let mut j = Journal::has_room(&[-10, -10]);
    let before = j.append(mce(None, Some(0), BankType::LoadStore).into(), &[]);
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-10, -10],
        900,
    );
    let (intent, trial) = j.intent(&plan);
    let other = TrialId::from("elsewhere".to_owned());
    let cases = [
        mce(Some(&trial), Some(0), BankType::DataFabric),
        mce(Some(&trial), None, BankType::LoadStore),
        mce(Some(&other), Some(0), BankType::LoadStore),
    ];
    let mut ignored = vec![before];
    for case in cases {
        ignored.push(j.append(case.into(), &[]));
    }
    assert_eq!(cited_core(&j, intent, &trial, &ignored), None);
    let kept = j.append(mce(Some(&trial), Some(1), BankType::LoadStore).into(), &[]);
    assert_eq!(cited_core(&j, intent, &trial, &ignored), None);
    assert_eq!(cited_core(&j, intent, &trial, &[kept]), Some(core(1)));
}

/// The core the attribution names when the open trial ends failed, citing `mces`, with no core of
/// its own.
pub(crate) fn cited_core(
    j: &Journal,
    intent: Seq,
    trial: &TrialId,
    mces: &[Seq],
) -> Option<togi_machine::CoreId> {
    let mut ended = j.resumed();
    let mut cause = vec![intent];
    cause.extend_from_slice(mces);
    ended.append(
        Journal::end_payload(trial, Outcome::Failure, Some(Signal::Crash)).into(),
        &cause,
    );
    let action = ended.next_decision();
    let DecisionPayload::Failure(failure) = action.payload else {
        panic!("expected the attribution, got {action:?}");
    };
    failure.core
}

#[test]
fn trial_progress_belongs_to_the_open_trial() {
    let mut j = Journal::search_at(&[-10]);
    let plan = j.next_trial();
    let (_, trial) = j.intent(&plan);
    let progress = |trial: &TrialId, signal| TrialProgress {
        trial: trial.clone(),
        detail: String::new(),
        signal,
        core: None,
    };
    j.append(progress(&trial, Some(Signal::Crash)).into(), &[]);
    let other = TrialId::from("elsewhere".to_owned());
    assert!(matches!(
        j.try_append(progress(&other, None).into(), &[]),
        Err(FoldError::NoOpenTrial(found)) if found == other
    ));
}

#[test]
fn carried_trials_name_a_workload_of_their_regime_or_the_first() {
    let j = Journal::has_room(&[-10]);
    let class = |workload| togi_journal::TrialClass {
        regime: Regime::R6,
        workload,
        cores: set(&[0]),
        duration_s: 900,
    };
    let carry = |class, outcome| {
        j.carried_trial(SESSION, class, Condition::Together, &[-10], outcome, None)
    };
    let mut ok = j.resumed();
    ok.append(carry(class(None), Outcome::Pass).into(), &[]);
    let mut wrong = j.resumed();
    let foreign = workloads(Regime::R1)[0].id();
    assert!(matches!(
        wrong.try_append(carry(class(Some(foreign)), Outcome::Pass).into(), &[]),
        Err(FoldError::WorkloadRegime { regime: Regime::R6, workload }) if workload == foreign
    ));
    let mut unsigned = j.resumed();
    let mut payload = carry(class(None), Outcome::Failure);
    payload.signal = None;
    assert!(matches!(
        unsigned.try_append(payload.into(), &[]),
        Err(FoldError::MissingSignal)
    ));
    let mut inconclusive = j.resumed();
    inconclusive.append(carry(class(None), Outcome::Inconclusive).into(), &[]);
    assert_eq!(inconclusive.state.project(), j.state.project());
    assert_eq!(inconclusive.state.next(), j.state.next());
}

#[test]
fn a_passed_full_cycle_ends_phase_1_only_once_every_core_left_search() {
    let mut j = Journal::search_at(&[-10]);
    j.profile_change(&[-10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    assert_eq!(j.run().phases.phase1_end(), None);

    let mut j = Journal::new(&[0], &[Start::solo(-10, -11)]);
    j.profile_change(&[-10]);
    j.cycle_start(1, &[Regime::R1]);
    let ended = j.cycle_end(1, true, true);
    j.cycle_end(2, true, true);
    assert_eq!(j.run().phases.phase1_end(), Some(ended));
    assert_eq!(j.run().phases.confirm_start(), None);
    assert_eq!(j.run().phases.concluded(), None);
}

#[test]
fn a_core_deeper_than_it_last_passed_is_checked_again_in_place() {
    let j = Journal::new(&[0, 1], &[Start::solo(-10, -13), Start::solo(-8, -8)]);
    let run = j.run();
    let passed = j.profile(&[-8, -8]);
    let moves = run.phase2_moves(&run.offsets(), Some(&passed)).unwrap();
    assert_eq!(moves.carried, [core(0)]);
    assert!(moves.moved.is_empty());
    assert_eq!(moves.profile, run.offsets());
}

#[test]
fn a_search_failure_keeps_a_shallower_failure_point_and_drops_a_contradicted_pass() {
    let mut j = Journal::new(&[0], &[Start::search(-10).fail(-5)]);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);
    let back = j.next_decision();
    let d = crate::schedule_tests::decision(&back);
    assert_eq!(d.failure_point, Some(off(-5)));

    let mut j = Journal::new(&[0], &[Start::search(-10).pass(-10)]);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);
    let back = j.next_decision();
    let d = crate::schedule_tests::decision(&back);
    assert_eq!(
        (d.pass, d.failure_point, d.to_offset),
        (None, Some(off(-10)), off(-5))
    );
    assert!(d.reason.contains("discarded"), "{}", d.reason);
}

#[test]
fn a_candidate_exit_is_judged_with_the_other_cores_where_they_stand() {
    let mut j = Journal::new(
        &[0, 1],
        &[Start::search(-20).checking(), Start::at_limit(-30)],
    );
    j.combination(1, &[(0, -19), (1, -30)]);
    let seen = j.run_until(
        100,
        |_| Outcome::Pass,
        |action| {
            matches!(action, Action::Decide(DecisionAction {
            payload: DecisionPayload::CorePhase(phase), ..
        }) if phase.core == core(0) && phase.to != CorePhase::Search)
        },
    );
    let Some(Action::Decide(DecisionAction {
        payload: DecisionPayload::CorePhase(exit),
        ..
    })) = seen.last()
    else {
        panic!("the check never ended");
    };
    assert_eq!(exit.offset, off(-19));
    assert_eq!(exit.to, CorePhase::AtLimit);
    assert!(exit.reason.contains("combination C1"), "{}", exit.reason);
}

#[test]
fn nothing_searches_when_no_core_is_in_search() {
    let j = Journal::has_room(&[-10]);
    assert_eq!(j.run().search_action(), None);
}

#[test]
fn a_known_failure_with_no_witness_yet_does_not_skip() {
    let mut j = Journal::search_at(&[-10]);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Failure);
    let again = j.manual(
        Regime::R1,
        0,
        Load::Single(core(0)),
        Condition::Alone,
        &[-10],
        90,
    );
    assert!(matches!(
        j.run().skip_known_failure(Action::RunTrial(again)),
        Action::RunTrial(_)
    ));
}

#[test]
fn a_skipped_round_check_names_its_round() {
    let mut j = Journal::search_at(&[-20]);
    j.carry(
        SESSION,
        (Regime::R1, 0, &[0], 90),
        Condition::Alone,
        &[-19],
        Outcome::Failure,
    );
    let mut plan = j.manual(
        Regime::R1,
        0,
        Load::Single(core(0)),
        Condition::Alone,
        &[-20],
        90,
    );
    plan.workload = WorkloadChoice::Frozen(workloads(Regime::R1)[0].id());
    plan.requirement.context = TrialContext::Round {
        round: NonZeroU32::new(3).unwrap(),
    };
    let Action::Decide(DecisionAction {
        payload: DecisionPayload::Failure(skip),
        ..
    }) = j.run().skip_known_failure(Action::RunTrial(plan))
    else {
        panic!("the round check was not skipped");
    };
    assert_eq!(skip.round, NonZeroU32::new(3));
}

#[test]
fn a_baseline_for_a_foreign_core_is_refused() {
    let mut j = Journal::search_at(&[-10]);
    let baseline = togi_journal::SessionBaseline {
        offsets: [(core(9), togi_machine::ObservedOffset(0))]
            .into_iter()
            .collect(),
    };
    assert!(matches!(
        j.try_append(baseline.into(), &[]),
        Err(FoldError::UnknownCore(found)) if found == core(9)
    ));
}

#[test]
fn only_a_failure_at_zero_naming_a_core_sets_a_failure_point() {
    let dead_end = |condition, core_id: Option<u8>| DeadEnd {
        condition,
        core: core_id.map(core),
        detail: String::new(),
        action: None,
    };
    let mut j = Journal::search_at(&[-10]);
    for case in [
        dead_end(DeadEndCondition::Smu, Some(0)),
        dead_end(DeadEndCondition::FailureAtZero, None),
    ] {
        j.append(case.into(), &[]);
        assert_eq!(j.run().failure_point(core(0)), None);
    }
    let seq = j.append(
        dead_end(DeadEndCondition::FailureAtZero, Some(0)).into(),
        &[],
    );
    assert_eq!(j.run().failure_point(core(0)), Some(off(0)));
    assert_eq!(j.run().core(core(0)).zero_seq, Some(seq));
    assert!(matches!(
        j.try_append(
            dead_end(DeadEndCondition::FailureAtZero, Some(9)).into(),
            &[]
        ),
        Err(FoldError::UnknownCore(found)) if found == core(9)
    ));
}

#[test]
fn a_reset_of_every_core_is_the_journals_business() {
    let mut j = Journal::search_at(&[-10]);
    let before = j.run().core(core(0));
    j.append(
        togi_journal::CommandReset {
            target: togi_journal::ResetTarget::All,
        }
        .into(),
        &[],
    );
    assert_eq!(j.run().core(core(0)), before);
    assert_eq!(j.run().core(core(0)).queued_reset, None);
}

#[test]
fn a_profile_change_naming_a_foreign_profile_is_refused() {
    let mut j = Journal::has_room(&[-10]);
    let foreign: PerCore<_> = [(core(9), off(-3))].into_iter().collect();
    let change = togi_journal::ProfileChange {
        from: Some(foreign),
        to: j.profile(&[-10]).offsets(),
    };
    assert!(matches!(
        j.try_append(change.into(), &[]),
        Err(FoldError::Profile(_))
    ));
}

#[test]
fn kinds_later_groups_own_change_no_tuner_state() {
    let mut j = Journal::search_at(&[-10]);
    let before = (j.run().core(core(0)), j.state.next().ok());
    let notice = togi_journal::SessionNotice {
        notice: Notice::NonzeroBaseline,
        cores: CoreSet::default(),
    };
    let ranking = togi_journal::HostRanking {
        ranking: vec![core(0)],
        values: PerCore::default(),
        detail: None,
    };
    let shutdown = togi_journal::Shutdown {
        reason: togi_journal::ShutdownReason::Command,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    };
    for payload in [Payload::from(notice), ranking.into(), shutdown.into()] {
        j.append(payload, &[]);
        assert_eq!((j.run().core(core(0)), j.state.next().ok()), before);
    }
    j.assert_resumes();
}

#[test]
fn the_checking_profile_follows_the_decided_offsets_only_when_it_differs() {
    let mut j = Journal::has_room(&[-10]);
    j.profile_change(&[-10]);
    assert!(j.run().profile_decision().is_none());
    j.decision(TunerDecision {
        core: core(0),
        activity: Activity::Checking,
        decision: Decision::Backoff,
        from_offset: off(-10),
        to_offset: off(-9),
        pass: None,
        failure_point: Some(off(-10)),
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    assert!(j.run().profile_decision().is_some());
}

#[test]
fn phase_2_moves_are_only_asked_of_profiles_of_the_topology() {
    let j = Journal::has_room(&[-10]);
    let foreign = crate::Profile::zero(&crate::tests::topology(&[4]));
    assert!(j.run().phase2_moves(&foreign, None).is_none());
}

fn crash_detected(intent: Seq) -> Payload {
    togi_journal::CrashDetected {
        previous_boot: togi_machine::BootId::from("boot".to_owned()),
        in_flight: Some(intent),
        stray: false,
        condition: None,
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        inconclusive: false,
        pstore: None,
    }
    .into()
}

#[test]
fn an_inconclusive_end_after_a_config_change_keeps_no_stale_retry() {
    let cases: Vec<(&str, Journal, &str)> = vec![
        (
            "search_trial_s",
            Journal::search_at(&[-10]),
            r#"{"durations":{"search_trial_s":60}}"#,
        ),
        (
            "evidence count",
            Journal::new(&[0], &[Start::search(-20).checking()]),
            r#"{"evidence":{"miss":0.001}}"#,
        ),
    ];
    for (name, mut j, json) in cases {
        let plan = j.next_trial();
        let (intent, trial) = j.intent(&plan);
        j.append(crash_detected(intent), &[]);
        j.load(&crate::testkit::config(json));
        j.end(intent, &trial, Outcome::Inconclusive);
        assert!(j.run().retry.is_none(), "{name}");
        let next = j.next_trial();
        assert!(!next.retry, "{name}");
        assert_eq!(next.duration, j.run().config.search_trial, "{name}");
        let intent = next.intent(TrialId::from("0002".to_owned()), j.rotation(&next));
        assert!(j.try_append(intent.into(), &next.cause).is_ok(), "{name}");
        assert_eq!(
            j.requirement().needed,
            u32::from(next.requirement.needed.get()),
            "{name}"
        );
        j.assert_resumes();
    }
}

#[test]
fn an_r2_retry_needs_the_backend_its_r1_passed_under() {
    use crate::testkit::{MPRIME_NEW, YCRUNCHER_NEW, backends};
    let mut j = Journal::search_at(&[-10]);
    let r1 = j.next_trial();
    assert_eq!(r1.regime, Regime::R1);
    j.trial(&r1, Outcome::Pass);
    let r2 = j.next_trial();
    assert_eq!(r2.regime, Regime::R2);
    j.trial(&r2, Outcome::Inconclusive);
    assert!(j.next_trial().retry);
    j.load(&backends(MPRIME_NEW, YCRUNCHER_NEW));
    let next = j.next_trial();
    assert_eq!((next.retry, next.regime), (false, Regime::R1));
    let intent = next.intent(TrialId::from("0003".to_owned()), j.rotation(&next));
    assert!(j.try_append(intent.into(), &next.cause).is_ok());
    j.assert_resumes();
}

#[test]
fn an_intent_while_a_trial_is_open_or_awaiting_attribution_is_refused() {
    let mut j = Journal::search_at(&[-10]);
    let plan = j.next_trial();
    let (intent, open) = j.intent(&plan);
    j.append(crash_detected(intent), &[]);
    let second = plan.intent(TrialId::from("0002".to_owned()), j.rotation(&plan));
    assert!(matches!(
        j.try_append(second.into(), &plan.cause),
        Err(FoldError::OverlappingIntent { trial, open: found })
            if trial == TrialId::from("0002".to_owned()) && found == open
    ));

    let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
    let plan = j.next_trial();
    let (intent, failed) = j.intent(&plan);
    j.end(intent, &failed, Outcome::Failure);
    assert!(attributes(&j, &failed));
    let second = plan.intent(TrialId::from("0002".to_owned()), j.rotation(&plan));
    assert!(matches!(
        j.try_append(second.into(), &plan.cause),
        Err(FoldError::OverlappingIntent { trial, open })
            if trial == TrialId::from("0002".to_owned()) && open == failed
    ));
}
