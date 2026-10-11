use std::cell::Cell;
use std::rc::Rc;

use togi_journal::{DeadEndCondition, Payload, SmuOp};
use togi_machine::{Condition, CoreId, Error, ObservedOffset, PerCore, TargetOffset};
use togi_sim::faults::{HookPhase, SmuOp as SimOp, SmuOperation};

use super::{
    ProfileFailure, ProfileRecord, ProfileRequest, Refusal, Refused, Slot, WriteFailure, apply,
    apply_profile,
};
use crate::emergency::{Access, Response, respond};
use crate::error::Flow;
use crate::testing::{Rig, assert_fold_consistent, core, target};

fn profile(offsets: &[i64]) -> PerCore<TargetOffset> {
    offsets
        .iter()
        .enumerate()
        .map(|(i, &offset)| (core(i64::try_from(i).unwrap()), target(offset)))
        .collect()
}

fn allow(_: &PerCore<TargetOffset>) -> Option<Refusal> {
    None
}

impl Rig {
    fn set(&mut self, n: i64, offset: i64) -> Result<super::Verified, WriteFailure> {
        let checked = self
            .hardware
            .check_set(core(n), target(offset), &mut allow)
            .unwrap();
        apply(
            &mut self.appender,
            &mut *self.smu,
            &self.stopped,
            checked,
            &[],
        )
    }

    fn zero_all(&mut self) -> Result<super::Verified, WriteFailure> {
        let checked = self.hardware.zero_all();
        apply(
            &mut self.appender,
            &mut *self.smu,
            &self.stopped,
            checked,
            &[],
        )
    }

    fn put(
        &mut self,
        offsets: &[i64],
        check: &mut dyn FnMut(&PerCore<TargetOffset>) -> Option<Refusal>,
    ) -> Result<super::Verified, ProfileFailure> {
        let target = profile(offsets);
        apply_profile(
            &mut self.appender,
            &mut *self.smu,
            &self.stopped,
            &mut self.hardware,
            ProfileRequest {
                target: &target,
                causes: &[],
                record: ProfileRecord::Applied(Condition::Together),
            },
            check,
        )
    }

    fn intents(&self) -> Vec<(Option<CoreId>, i64)> {
        self.events()
            .iter()
            .filter_map(|event| match &event.payload {
                Payload::SmuIntent(intent) => {
                    Some((intent.command().core(), intent.command().offset().get()))
                }
                _ => None,
            })
            .collect()
    }
}

#[test]
fn a_write_is_intent_then_write_then_readback() {
    let mut rig = Rig::new(4);
    rig.start_at_zero();
    let start = rig.kinds().len();
    let verified = rig.set(0, -10).unwrap();
    assert_eq!(
        rig.kinds()[start..],
        ["smu.intent", "smu.write", "smu.readback"]
    );
    let events = &rig.events()[start..];
    assert_eq!(events[1].cause, [events[0].seq]);
    assert_eq!(events[2].cause, [events[1].seq]);
    assert_eq!(verified.readbacks(), [events[2].seq]);
    assert_eq!(verified.targets(), [(core(0), target(-10))]);
    assert_eq!(rig.offset(0), -10);
    assert_eq!(rig.hardware.slot(core(0)), Some(Slot::Known(target(-10))));
    assert_fold_consistent(&rig.appender);
}

#[test]
fn zeroing_every_core_reads_every_core_back() {
    let mut rig = Rig::new(4);
    let verified = rig.zero_all().unwrap();
    assert_eq!(verified.readbacks().len(), 4);
    assert_eq!(
        rig.kinds().iter().filter(|k| **k == "smu.readback").count(),
        4
    );
    let profile = rig.hardware.profile().unwrap();
    assert!(
        profile
            .iter()
            .all(|(_, offset)| offset == TargetOffset::ZERO)
    );
    assert_fold_consistent(&rig.appender);
}

#[test]
fn a_refused_write_appends_no_intent_and_reaches_no_hardware() {
    let mut rig = Rig::new(4);
    rig.zero_all().unwrap();
    let before = rig.kinds();
    let refused = rig
        .hardware
        .check_set(core(1), target(-30), &mut |_| {
            Some(Refusal::new("failure point -30 of core 01"))
        })
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "refusing to write core 01 to -30: the profile would reach failure point -30 of core 01"
    );
    assert!(matches!(refused, Refused::Reaches { .. }));
    assert_eq!(rig.kinds(), before);
    assert_eq!(rig.offset(1), 0);
    assert_eq!(
        rig.hardware.slot(core(1)),
        Some(Slot::Known(TargetOffset::ZERO))
    );
}

#[test]
fn only_a_write_that_can_deepen_a_core_is_checked() {
    let mut rig = Rig::new(2);
    rig.zero_all().unwrap();
    rig.set(0, -20).unwrap();
    let seen = Rc::new(Cell::new(0_u32));
    let counter = Rc::clone(&seen);
    let mut count = move |_: &PerCore<TargetOffset>| {
        counter.set(counter.get() + 1);
        None
    };
    drop(
        rig.hardware
            .check_set(core(0), target(-10), &mut count)
            .unwrap(),
    );
    drop(
        rig.hardware
            .check_set(core(0), target(0), &mut count)
            .unwrap(),
    );
    drop(
        rig.hardware
            .check_set(core(0), target(-20), &mut count)
            .unwrap(),
    );
    assert_eq!(seen.get(), 0);
    drop(
        rig.hardware
            .check_set(core(0), target(-21), &mut count)
            .unwrap(),
    );
    assert_eq!(seen.get(), 1);
}

#[test]
fn the_check_sees_the_profile_the_write_would_leave() {
    let mut rig = Rig::new(2);
    rig.zero_all().unwrap();
    rig.set(0, -20).unwrap();
    let mut seen = None;
    let checked = rig
        .hardware
        .check_set(core(1), target(-5), &mut |after| {
            seen = Some(*after);
            None
        })
        .unwrap();
    drop(checked);
    assert_eq!(seen, Some(profile(&[-20, -5])));
}

#[test]
fn a_negative_write_over_unknown_offsets_is_refused() {
    let mut rig = Rig::new(2);
    let refused = rig
        .hardware
        .check_set(core(0), target(-5), &mut allow)
        .unwrap_err();
    assert_eq!(refused, Refused::Unknown { core: core(0) });
    assert_eq!(rig.kinds(), ["session.start"]);
}

#[test]
fn a_write_to_a_core_the_machine_lacks_is_refused() {
    let mut rig = Rig::new(2);
    let refused = rig
        .hardware
        .check_set(core(9), target(-5), &mut allow)
        .unwrap_err();
    assert_eq!(refused, Refused::NoSuchCore { core: core(9) });
}

#[test]
fn a_profile_zeroes_an_unknown_machine_then_writes_shallower_before_deeper() {
    let mut rig = Rig::new(4);
    rig.put(&[-30, -20, -10, -30], &mut allow).unwrap();
    assert_eq!(rig.intents()[0], (None, 0));
    let start = rig.intents().len();
    let verified = rig.put(&[-31, -19, -11, -29], &mut allow).unwrap();
    let steps: Vec<_> = rig.intents()[start..]
        .iter()
        .map(|(core, offset)| (core.unwrap().get(), *offset))
        .collect();
    assert_eq!(steps, [(1, -19), (3, -29), (0, -31), (2, -11)]);
    let record = verified.profile().unwrap();
    let event = rig.appender.journal().event(record).unwrap();
    assert_eq!(event.kind().as_str(), "profile.applied");
    assert_eq!(event.cause, verified.readbacks());
    assert_eq!(verified.readbacks().len(), 4);
    assert_fold_consistent(&rig.appender);
}

#[test]
fn a_profile_that_is_already_on_the_machine_writes_nothing() {
    let mut rig = Rig::new(2);
    rig.put(&[-5, -6], &mut allow).unwrap();
    let writes = rig.intents().len();
    let verified = rig.put(&[-5, -6], &mut allow).unwrap();
    assert_eq!(rig.intents().len(), writes);
    assert!(verified.readbacks().is_empty());
}

#[test]
fn a_combination_is_refused_at_the_step_that_reaches_it_with_the_earlier_steps_kept() {
    let mut rig = Rig::new(4);
    let mut combination = |after: &PerCore<TargetOffset>| {
        let deep = |n| after.get(core(n)).is_some_and(|offset| offset.get() <= -30);
        (deep(0) && deep(1)).then(|| Refusal::new("combination 1"))
    };
    let error = rig.put(&[-30, -30, 0, 0], &mut combination).unwrap_err();
    assert!(matches!(
        error,
        ProfileFailure::Refused(Refused::Reaches { .. })
    ));
    assert_eq!(rig.intents(), [(None, 0), (Some(core(0)), -30)]);
    assert_eq!(rig.offset(0), -30);
    assert_eq!(rig.offset(1), 0);
    assert!(!rig.kinds().contains(&"profile.applied"));
    assert_eq!(rig.hardware.slot(core(0)), Some(Slot::Known(target(-30))));
    assert_eq!(
        rig.hardware.slot(core(1)),
        Some(Slot::Known(TargetOffset::ZERO))
    );
    assert_fold_consistent(&rig.appender);
}

#[test]
fn a_failure_point_is_refused_before_any_write_of_that_core() {
    let mut rig = Rig::new(4);
    rig.put(&[0, 0, 0, 0], &mut allow).unwrap();
    let before = rig.kinds();
    let mut failure_point = |after: &PerCore<TargetOffset>| {
        after
            .get(core(2))
            .is_some_and(|offset| offset.get() <= -30)
            .then(|| Refusal::new("failure point -30 of core 02"))
    };
    let error = rig.put(&[0, 0, -30, 0], &mut failure_point).unwrap_err();
    assert_eq!(
        error.to_string(),
        "refusing to write core 02 to -30: the profile would reach failure point -30 of core 02"
    );
    assert_eq!(rig.kinds(), before);
    assert_eq!(
        rig.hardware.slot(core(2)),
        Some(Slot::Known(TargetOffset::ZERO))
    );
}

#[test]
fn a_profile_must_name_exactly_the_cores_of_the_machine() {
    let mut rig = Rig::new(4);
    let error = rig.put(&[-5, -5], &mut allow).unwrap_err();
    assert!(matches!(
        error,
        ProfileFailure::Shape {
            profile: 2,
            cores: 4
        }
    ));
    assert_eq!(rig.kinds(), ["session.start"]);
}

#[test]
fn a_failed_command_is_recorded_and_is_an_smu_dead_end_with_the_verified_steps_kept() {
    let mut rig = Rig::new(4);
    rig.sim.fail_write(3);
    let error = rig.put(&[-30, -30, 0, 0], &mut allow).unwrap_err();
    let ProfileFailure::Write(
        failure @ WriteFailure::Smu {
            op, core: failed, ..
        },
    ) = error
    else {
        panic!("{error:?}");
    };
    assert_eq!((op, failed), (SmuOp::Set, Some(core(1))));
    assert!(matches!(
        Flow::from(failure),
        Flow::DeadEnd(DeadEndCondition::Smu)
    ));
    let events = rig.events();
    let record = events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SmuError(error) => Some((event.cause.clone(), error.clone())),
            _ => None,
        })
        .unwrap();
    let last_intent = events
        .iter()
        .rev()
        .find(|event| event.kind().as_str() == "smu.intent")
        .unwrap()
        .seq;
    assert_eq!(record.0, [last_intent]);
    assert_eq!(
        (record.1.op, record.1.core, record.1.offset),
        (SmuOp::Set, Some(core(1)), Some(target(-30)))
    );
    assert_eq!(rig.hardware.slot(core(0)), Some(Slot::Known(target(-30))));
    assert_eq!(rig.hardware.slot(core(1)), Some(Slot::Unknown));
    assert!(!rig.kinds().contains(&"profile.applied"));
    assert!(
        rig.appender
            .recovery()
            .smu_evidence()
            .unwrap()
            .detail
            .starts_with("SMU command failed")
    );
    assert_fold_consistent(&rig.appender);
}

#[test]
fn a_readback_that_disagrees_is_durable_then_an_smu_dead_end() {
    let mut rig = Rig::new(2);
    rig.zero_all().unwrap();
    rig.sim.corrupt_readback(core(0));
    let failure = rig.set(0, -10).unwrap_err();
    let WriteFailure::Mismatch {
        core: bad,
        observed,
        expected,
    } = &failure
    else {
        panic!("{failure:?}");
    };
    assert_eq!(
        (*bad, *observed, *expected),
        (core(0), ObservedOffset(-11), target(-10))
    );
    assert_eq!(rig.kinds().last(), Some(&"smu.readback"));
    assert_eq!(rig.hardware.slot(core(0)), Some(Slot::Unknown));
    assert_eq!(
        rig.appender.recovery().smu_evidence().unwrap().detail,
        "core 00 reads CO -11 after writing -10"
    );
    assert!(matches!(
        Flow::from(failure),
        Flow::DeadEnd(DeadEndCondition::Smu)
    ));
    assert_fold_consistent(&rig.appender);
}

#[test]
fn a_journal_failure_at_the_intent_leaves_the_hardware_alone() {
    let mut rig = Rig::new(2);
    rig.zero_all().unwrap();
    let before = rig.kinds();
    rig.fail_journal_write(1);
    let failure = rig.set(0, -10).unwrap_err();
    assert!(matches!(failure, WriteFailure::Journal(_)));
    assert_eq!(rig.offset(0), 0);
    assert_eq!(rig.kinds(), before);
    assert!(rig.appender.failure().is_some());
}

#[test]
fn a_failed_write_event_leaves_no_readback_and_hands_off_to_the_emergency() {
    let mut rig = Rig::new(4);
    rig.zero_all().unwrap();
    let reads = Rc::new(Cell::new(0_u32));
    let counter = Rc::clone(&reads);
    rig.sim.interrupt_smu(move |operation: &SmuOperation| {
        if operation.op == SimOp::Read {
            counter.set(counter.get() + 1);
        }
        Ok::<(), Error>(())
    });
    rig.fail_journal_write(2);
    let failure = rig.set(0, -10).unwrap_err();
    let WriteFailure::Journal(journal) = failure else {
        panic!("{failure:?}");
    };
    assert_eq!(reads.get(), 0);
    assert_eq!(rig.offset(0), -10);
    let events_before = rig.kinds();
    assert_eq!(events_before.last(), Some(&"smu.intent"));
    assert_eq!(rig.hardware.slot(core(0)), Some(Slot::Unknown));

    let mut report = Vec::new();
    let gate = Access::gate(Some(rig.sweep), Some(rig.validated), Some(&rig.stopped));
    let cores = rig.hardware.cores();
    let outcome = respond(&journal, gate, &mut *rig.smu, cores, &mut report);
    assert!(matches!(&outcome.response, Response::Zeroed(zeroed) if zeroed.is_clean()));
    assert_eq!(rig.offset(0), 0);
    assert_eq!(rig.kinds(), events_before);
    assert!(String::from_utf8(report).unwrap().contains("journal write"));
}

#[test]
fn a_failed_readback_event_leaves_the_slot_unconfirmed() {
    let mut rig = Rig::new(2);
    rig.zero_all().unwrap();
    rig.fail_journal_write(3);
    let failure = rig.set(1, -10).unwrap_err();
    assert!(matches!(failure, WriteFailure::Journal(_)));
    assert_eq!(rig.kinds().last(), Some(&"smu.write"));
    assert_eq!(rig.offset(1), -10);
    assert_eq!(rig.hardware.slot(core(1)), Some(Slot::Unknown));
}

#[test]
fn the_first_failed_append_latches_and_cancels_the_running_trial() {
    let mut rig = Rig::new(2);
    let token = togi_machine::CancellationToken::new();
    rig.appender.watch_trial(Some(token.clone()));
    rig.fail_journal_write(1);
    let first = rig.zero_all().unwrap_err();
    assert!(token.is_cancelled());
    let before = rig.kinds();
    let second = rig
        .appender
        .append(
            Payload::SessionWarning(togi_journal::SessionWarning {
                operation: "later".into(),
                trial: None,
                error: String::new(),
            }),
            &[],
        )
        .unwrap_err();
    assert_eq!(rig.kinds(), before);
    let (WriteFailure::Journal(first), crate::AppendFailure::Journal(second)) = (first, second)
    else {
        panic!("not journal failures");
    };
    assert_eq!(first.to_string(), second.to_string());
}

#[test]
fn a_crash_at_every_smu_access_leaves_the_intent_and_no_invented_evidence() {
    let cases = [
        (SimOp::Set, HookPhase::Before, 1),
        (SimOp::Set, HookPhase::After, 1),
        (SimOp::Read, HookPhase::Before, 2),
        (SimOp::Read, HookPhase::After, 2),
    ];
    for (op, phase, added) in cases {
        let mut rig = Rig::new(2);
        rig.start_at_zero();
        let mut expected = rig.kinds();
        expected.extend(["smu.intent", "smu.write"].into_iter().take(added));
        let intent = rig
            .appender
            .journal()
            .events()
            .last()
            .unwrap()
            .seq
            .next()
            .unwrap();
        rig.crash_at(op, phase);
        let failure = rig.set(0, -10).unwrap_err();
        assert!(matches!(failure, WriteFailure::Crashed), "{op:?} {phase:?}");
        assert_eq!(rig.kinds(), expected, "{op:?} {phase:?}");
        assert!(matches!(Flow::from(failure), Flow::Fatal(_)));
        assert_eq!(rig.hardware.slot(core(0)), Some(Slot::Unknown));
        let pending = rig.appender.recovery().latest_unmatched_intent(&rig.boot);
        assert_eq!(pending, (added == 1).then_some(intent), "{op:?} {phase:?}");
        assert_fold_consistent(&rig.appender);
    }
}

#[test]
fn a_crash_at_the_write_is_before_any_effect() {
    let mut rig = Rig::new(2);
    rig.start_at_zero();
    let mut expected = rig.kinds();
    expected.push("smu.intent");
    rig.sim.crash_at_write(1);
    let failure = rig.set(0, -10).unwrap_err();
    assert!(matches!(failure, WriteFailure::Crashed));
    assert_eq!(rig.kinds(), expected);
    let registers = rig.appender.recovery().registers(&rig.boot).unwrap();
    assert!(registers.uncertain(core(0)));
    assert_eq!(registers.offset(core(0)), Some(ObservedOffset(-10)));
    assert_fold_consistent(&rig.appender);
}

fn tuner_after(rig: &mut Rig, payloads: Vec<Payload>) -> togi_tuner::State {
    for payload in payloads {
        rig.appender.append(payload, &[]).unwrap();
    }
    let mut state = togi_tuner::State::new();
    for event in rig.events() {
        state.fold(event).unwrap();
    }
    state
}

fn failure_point(core_id: i64, offset: i64) -> Payload {
    Payload::CorePhase(togi_journal::CoreTransition {
        core: core(core_id),
        from: None,
        to: togi_journal::CorePhase::AtLimit,
        offset: target(offset + 1),
        pass: Some(target(offset + 1)),
        failure_point: Some(target(offset)),
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    })
}

fn combination(members: &[(i64, i64)]) -> Payload {
    Payload::Combination(togi_journal::Combination {
        combination: std::num::NonZeroU32::MIN,
        members: members
            .iter()
            .map(|&(member, offset)| togi_journal::Member {
                core: core(member),
                offset: target(offset),
            })
            .collect(),
        fallback: false,
        hunt: std::num::NonZeroU32::MIN,
        reason: "test".to_owned(),
    })
}

#[test]
fn a_write_reaching_a_folded_failure_point_is_refused_with_no_intent_appended() {
    let mut rig = Rig::new(4);
    rig.zero_all().unwrap();
    let state = tuner_after(&mut rig, vec![failure_point(2, -30)]);
    let before = rig.kinds();
    for deeper in [-30, -31, -50] {
        let refused = rig
            .hardware
            .check_set(core(2), target(deeper), &mut |after| {
                super::refusal(&state, after)
            })
            .unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "refusing to write core 02 to {deeper}: the profile would reach failure point -30 of core 02"
            )
        );
    }
    assert_eq!(rig.kinds(), before);
    assert_eq!(rig.offset(2), 0);
    drop(
        rig.hardware
            .check_set(core(2), target(-29), &mut |after| {
                super::refusal(&state, after)
            })
            .unwrap(),
    );
}

#[test]
fn a_write_completing_a_folded_combination_is_refused_after_the_safe_steps() {
    let mut rig = Rig::new(4);
    rig.zero_all().unwrap();
    let state = tuner_after(&mut rig, vec![combination(&[(0, -20), (1, -20)])]);
    let error = rig
        .put(&[-20, -20, 0, 0], &mut |after| {
            super::refusal(&state, after)
        })
        .unwrap_err();
    let ProfileFailure::Refused(Refused::Reaches {
        core: refused,
        refusal,
        ..
    }) = error
    else {
        panic!("not refused: {error:?}");
    };
    assert_eq!(refused, core(1));
    assert_eq!(refusal.to_string(), "combination C1");
    assert_eq!(rig.intents(), [(None, 0), (Some(core(0)), -20)]);
    assert_eq!(rig.offset(0), -20);
    assert_eq!(rig.offset(1), 0);
}

#[test]
fn a_profile_the_tuner_cannot_judge_is_refused_never_read_as_safe() {
    let mut rig = Rig::new(4);
    let empty = togi_tuner::State::new();
    assert!(super::refusal(&empty, &profile(&[0, 0, 0, 0])).is_some());
    let state = tuner_after(&mut rig, Vec::new());
    assert!(super::refusal(&state, &profile(&[0, 0, 0, 0])).is_none());
    assert!(super::refusal(&state, &profile(&[0, 0, 0])).is_some());
    assert!(super::refusal(&state, &profile(&[0, 0, 0, 0, 0])).is_some());
}
