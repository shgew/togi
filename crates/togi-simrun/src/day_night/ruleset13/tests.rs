use std::time::Duration;

use togi_journal::{
    ApplyCause, Event, Outcome, Payload, QualificationState, ShutdownReason, StepKind,
};
use togi_machine::{CcdProfile, Machine};
use togi_tuner::policy::{Params, State};

use super::Ruleset13;
use crate::day_night::{
    CalendarConfig, DayClassification, DayObservation, DayStart, EndAnswer, Night, Policy,
    PolicyKind, RunError, run_controlled, run_with_policy,
};

const SCENARIO: &str = "edge-deep";

fn calendar(days: u32) -> CalendarConfig {
    CalendarConfig {
        days,
        ..CalendarConfig::default()
    }
}

/// Days enough for the policy's first promotion on [`SCENARIO`] (the first lands in the fourth week).
const PROMOTION_DAYS: u32 = 30;

/// The longest a controlled run may take; each stops at its transition well before.
const DAYS_BOUND: u32 = 365;

/// Runs `policy` on [`SCENARIO`]'s seed 1, asking `start` before each day, and fails unless
/// `start` ends the run within [`DAYS_BOUND`] days.
fn run_until<P: Policy>(policy: &mut P, mut start: impl FnMut(&P) -> DayStart) {
    let mut stopped = false;
    run_controlled(SCENARIO, 1, calendar(DAYS_BOUND), policy, |_, policy| {
        let next = start(policy);
        stopped |= next == DayStart::Stop;
        next
    })
    .unwrap();
    assert!(
        stopped,
        "the asserted transition did not complete within {DAYS_BOUND} days"
    );
}

/// Whether `policy` applies a nonzero day profile.
fn holds_promotion(policy: &Ruleset13) -> bool {
    policy
        .state()
        .project()
        .applied
        .is_some_and(|profile| !profile.is_zero())
}

#[test]
fn the_gate_policy_builds_from_its_slope_and_refuses_a_non_multiplier() {
    let kind = PolicyKind::Ruleset13 {
        fallback_slope: 2.0,
    };
    assert!(kind.build(&calendar(1)).is_ok());
    for slope in [1.0, 0.5, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            Ruleset13::new(slope),
            Err(RunError::Slope(rejected)) if rejected.to_bits() == slope.to_bits()
        ));
    }
}

#[test]
fn the_journal_folds_identically_when_replayed() {
    let mut policy = Ruleset13::new(1.5).unwrap().recording();
    run_with_policy(SCENARIO, 1, calendar(PROMOTION_DAYS), &mut policy).unwrap();
    let mut replayed = State::new(Params {
        fallback_slope: togi_tuner::cap::Beta::new(1.5_f64.ln()).unwrap(),
        ..Params::SPEC
    });
    for event in policy.events().unwrap() {
        replayed.fold(event).unwrap();
    }
    assert_eq!(
        serde_json::to_string(&replayed.project()).unwrap(),
        serde_json::to_string(&policy.state().project()).unwrap()
    );
    let seqs: Vec<u64> = policy
        .events()
        .unwrap()
        .iter()
        .map(|event| event.seq.get())
        .collect();
    let expected: Vec<u64> = (1..).take(seqs.len()).collect();
    assert_eq!(seqs, expected);
}

#[test]
fn an_adapter_not_recording_keeps_no_journal_and_decides_alike() {
    let mut recording = Ruleset13::new(1.5).unwrap().recording();
    run_with_policy(SCENARIO, 1, calendar(3), &mut recording).unwrap();
    let mut bare = Ruleset13::new(1.5).unwrap();
    run_with_policy(SCENARIO, 1, calendar(3), &mut bare).unwrap();
    assert!(bare.events().is_none());
    assert!(!recording.events().unwrap().is_empty());
    assert_eq!(
        serde_json::to_string(&bare.state().project()).unwrap(),
        serde_json::to_string(&recording.state().project()).unwrap()
    );
}

#[test]
fn a_first_promotion_follows_a_completed_night_qualification() {
    let mut policy = Ruleset13::new(1.5).unwrap().recording();
    run_until(&mut policy, |policy| {
        if holds_promotion(policy) {
            DayStart::Stop
        } else {
            DayStart::Run
        }
    });
    let events = policy.events().unwrap();
    let promoted_by = |decision| {
        events.iter().any(|step| {
            matches!(&step.payload, Payload::DayStep(step)
                if step.decision == decision && matches!(step.step, StepKind::Promote { .. }))
        })
    };
    let promotion = events
        .iter()
        .position(|event| {
            if let Payload::DayApplied(applied) = &event.payload
                && let ApplyCause::Step { decision } = applied.cause
            {
                promoted_by(decision)
            } else {
                false
            }
        })
        .unwrap();
    let qualified = events[..promotion].iter().any(|event| {
        matches!(&event.payload, Payload::NightQualification(q)
            if matches!(q.state, QualificationState::Complete { .. }))
    });
    assert!(
        qualified,
        "promoted at event {promotion} before any qualification completed"
    );
}

fn is_new_firmware(version: &str) -> bool {
    version.ends_with("-transition")
}

fn applied(event: &Event) -> Option<CcdProfile> {
    if let Payload::DayApplied(applied) = &event.payload {
        Some(applied.target.ccd().clone())
    } else {
        None
    }
}

/// Splits `events` at the first session context on the new firmware.
fn at_firmware_change(events: &[Event]) -> Option<(&[Event], &[Event])> {
    let changed = events.iter().position(|event| {
        matches!(&event.payload, Payload::SessionContext(context) if is_new_firmware(&context.bios_version))
    })?;
    Some(events.split_at(changed))
}

/// Whether `events` completed a qualification of `profile` under the new firmware.
fn requalified(profile: &CcdProfile, events: &[Event]) -> bool {
    events.iter().any(|event| {
        matches!(&event.payload, Payload::NightQualification(q)
            if is_new_firmware(&q.firmware.bios_version)
                && q.profile.ccd() == profile
                && matches!(q.state, QualificationState::Complete { .. }))
    })
}

/// The last nonzero profile applied before the firmware changed.
fn previous_profile(before: &[Event]) -> Option<CcdProfile> {
    before
        .iter()
        .rev()
        .find_map(applied)
        .filter(|profile| !profile.is_zero())
}

#[test]
fn a_firmware_change_resets_a_nonzero_profile_to_stock_until_requalified() {
    let mut policy = Ruleset13::new(1.5).unwrap().recording();
    // The firmware changes once a promotion holds, and the run ends once the promoted profile is
    // qualified afresh under the new firmware and a nonzero profile is applied again.
    let mut changed = false;
    run_until(&mut policy, |policy| {
        if !changed {
            changed = holds_promotion(policy);
            return if changed {
                DayStart::ChangeFirmware
            } else {
                DayStart::Run
            };
        }
        let events = policy.events().unwrap();
        let returned = at_firmware_change(events).is_some_and(|(before, after)| {
            previous_profile(before).is_some_and(|previous| requalified(&previous, after))
                && after
                    .iter()
                    .filter_map(applied)
                    .any(|profile| !profile.is_zero())
        });
        if returned {
            DayStart::Stop
        } else {
            DayStart::Run
        }
    });
    let events = policy.events().unwrap();
    let (before, after) =
        at_firmware_change(events).expect("the calendar recorded no firmware change");
    let previous =
        previous_profile(before).expect("no nonzero profile was applied when the firmware changed");
    // The next cold boot proposes the firmware zero and applies stock.
    let step = after
        .iter()
        .find_map(|event| {
            if let Payload::DayStep(step) = &event.payload {
                Some(step)
            } else {
                None
            }
        })
        .unwrap();
    assert!(
        matches!(step.step, StepKind::FirmwareZero { .. }),
        "{:?}",
        step.step
    );
    assert!(after.iter().find_map(applied).unwrap().is_zero());
    // The old qualification no longer counts: the previous profile is qualified afresh under the
    // new firmware, and no nonzero profile returns without such a qualification of its own.
    assert!(
        requalified(&previous, after),
        "{previous:?} was never qualified under the new firmware"
    );
    for (index, event) in after.iter().enumerate() {
        if let Some(profile) = applied(event).filter(|profile| !profile.is_zero()) {
            assert!(
                requalified(&profile, &after[..index]),
                "{profile:?} returned at event {} without a qualification under the new firmware",
                event.seq
            );
        }
    }
}

/// Delegates to [`Ruleset13`], replacing the first day observation after a promotion with an end the
/// operator answers as a freeze.
struct FreezeAfterPromotion {
    inner: Ruleset13,
    frozen: Option<DayClassification>,
}

impl Policy for FreezeAfterPromotion {
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.inner.firmware_changed(machine)
    }
    fn start_day(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.inner.start_day(day, machine)
    }
    fn observe_day(
        &mut self,
        day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        let promoted = holds_promotion(&self.inner);
        if promoted && self.frozen.is_none() {
            let freeze = DayObservation::Ended {
                active: Duration::from_hours(1),
                answer: Some(EndAnswer::Cpu),
            };
            let classification = self.inner.observe_day(day, &freeze, machine)?;
            self.frozen = Some(classification);
            return Ok(classification);
        }
        self.inner.observe_day(day, evidence, machine)
    }
    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.inner.night(night, machine)
    }
}

#[test]
fn a_cpu_suspect_answer_leads_to_a_retreat() {
    let mut policy = FreezeAfterPromotion {
        inner: Ruleset13::new(1.5).unwrap().recording(),
        frozen: None,
    };
    // The run ends once a retreat step follows the CPU-suspect event.
    run_until(&mut policy, |policy| {
        let retreated = policy.inner.events().unwrap().iter().any(|event| {
            matches!(&event.payload, Payload::DayStep(step)
                if matches!(step.step, StepKind::Retreat { .. }))
        });
        if policy.frozen.is_some() && retreated {
            DayStart::Stop
        } else {
            DayStart::Run
        }
    });
    assert_eq!(policy.frozen, Some(DayClassification::CpuSuspect));
    let report = policy.inner.report();
    assert_eq!(report.allowance, None);
    assert_eq!(report.finishes, Vec::new());
    assert!(report.events.iter().any(|event|
        event.classification == crate::day_night::successor::Classification::CpuSuspect));
    assert!(report.events.iter().all(|event| !event.charged));
}

#[test]
fn the_operator_ends_a_night_mid_trial_by_signal_before_the_next_day_boot() {
    // A 36 s night cuts the first trial, which is at least 90 s long.
    let calendar = CalendarConfig {
        days: 2,
        night_hours: 0.01,
        ..CalendarConfig::default()
    };
    let mut policy = Ruleset13::new(1.5).unwrap().recording();
    run_with_policy(SCENARIO, 1, calendar, &mut policy).unwrap();
    let events = policy.events().unwrap();
    let (end, trial) = events
        .iter()
        .enumerate()
        .find_map(|(index, event)| {
            if let Payload::TrialEnd(trial) = &event.payload {
                Some((index, trial))
            } else {
                None
            }
        })
        .unwrap();
    let (shutdown, stop) = events
        .iter()
        .enumerate()
        .find_map(|(index, event)| {
            if let Payload::Shutdown(stop) = &event.payload {
                Some((index, stop))
            } else {
                None
            }
        })
        .unwrap();
    assert!(trial.interrupted);
    assert_eq!(trial.outcome, Outcome::Inconclusive);
    assert_eq!(trial.duration_s, 36);
    assert_eq!(
        trial.reason.as_deref(),
        Some("trial ended before its full duration; stopped by signal")
    );
    assert_eq!(stop.reason, ShutdownReason::Signal);
    // Nothing else ran that night, and the tuning boot closed before the next day boot started.
    assert_eq!(shutdown, end + 1);
    let starts = events[..shutdown]
        .iter()
        .filter(|event| matches!(event.payload, Payload::TrialStart(_)))
        .count();
    assert_eq!(starts, 1);
    let next_day = events[shutdown..]
        .iter()
        .position(|event| matches!(event.payload, Payload::DayBootStart(_)));
    assert!(next_day.is_some(), "no day boot followed the night");
}
