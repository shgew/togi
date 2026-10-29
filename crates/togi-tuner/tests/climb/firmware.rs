use super::*;

fn saved_fixture() -> (Fixture, UnitProfile) {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.advance();
    let saved = f.firmware_zero();
    (f, saved)
}

#[test]
fn a_firmware_change_resets_to_stock_and_keeps_the_allowance_and_stops() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.event("stopped", cpu());
    f.apply_retreat(&[-5, 0]);
    let campaign = f.campaign.clone();
    let stops = f.history.finishes().clone();
    let steps = f.history.steps().to_vec();
    let saved = f.firmware_zero();
    assert!(f.history.current().is_zero());
    assert_eq!(f.history.saved().unwrap().profile, saved);
    assert_eq!(f.history.finishes(), &stops);
    assert_eq!(f.campaign, campaign);
    assert_eq!(f.history.steps(), steps);
}

#[test]
fn restoration_needs_the_exact_previous_vector_qualified_in_the_new_context() {
    let (mut f, saved) = saved_fixture();
    f.hold = HOLD_SECS;
    assert!(blocked(&f.decide(), &climb::Blocker::Certificate));
    f.qualified.insert(profile(f.history.units(), &[-15, -10]));
    assert!(blocked(&f.decide(), &climb::Blocker::Certificate));
    f.qualified.insert(saved.clone());
    assert_eq!(f.decide().target(), Some(&saved));
    assert!(matches!(f.decide().action, ClimbAction::Restore(_)));
}

#[test]
fn restoration_needs_fifty_clean_stock_hours() {
    let (mut f, saved) = saved_fixture();
    f.qualified.insert(saved);
    f.hold = HOLD_SECS - 60;
    assert!(blocked(
        &f.decide(),
        &climb::Blocker::Hold {
            credited_secs: HOLD_SECS - 60,
            required_secs: HOLD_SECS
        }
    ));
    f.hold = HOLD_SECS;
    assert!(matches!(f.decide().action, ClimbAction::Restore(_)));
}

#[test]
fn restoration_applies_the_whole_previous_profile_in_one_learning_step() {
    let (mut f, saved) = saved_fixture();
    let before_count = f.history.steps().len();
    f.qualified.insert(saved.clone());
    f.hold = HOLD_SECS;
    // No newly mapped edges is the explicit restoration exception, not permission to re-climb.
    f.ceilings.clear();
    let decision = f.decide();
    assert_eq!(decision.target(), Some(&saved));
    f.apply(&decision);
    assert_eq!(f.history.steps().len(), before_count + 1);
    let restored = f.history.steps().last().unwrap();
    assert_eq!(restored.kind, climb::StepKind::FirmwareRestoration);
    assert!(restored.before.profile.is_zero());
    assert_eq!(restored.after.profile, saved);
    assert_eq!(restored.changed.len(), 2);
}

#[test]
fn an_event_after_restoration_steps_back_to_stock_and_stops_the_restored_units() {
    let (mut f, saved) = saved_fixture();
    f.qualified.insert(saved);
    f.hold = HOLD_SECS;
    let restore = f.decide();
    f.apply(&restore);
    f.event(
        "restoration",
        Classification::ProvisionalCpuSuspect(togi_tuner::day::ProvisionalCause::UnsureAnswer),
    );
    let decision = f.decide();
    assert!(decision.target().unwrap().is_zero());
    f.apply(&decision);
    assert!(f.history.current().iter().all(|(unit, _)| matches!(
        f.history.finishes().get(unit).unwrap().reason,
        climb::FinishReason::Stopped(climb::StopReason::RestorationBackstep { .. })
    )));
    assert_eq!(f.campaign.consumed(), Some(1));
}

#[test]
fn a_previously_qualified_saved_profile_alone_never_promotes() {
    let (mut f, saved) = saved_fixture();
    f.qualified.insert(saved);
    assert!(blocked(
        &f.decide(),
        &climb::Blocker::Hold {
            credited_secs: 0,
            required_secs: HOLD_SECS
        }
    ));
    assert!(f.history.current().is_zero());
}

#[test]
fn a_nonuniform_saved_core_vector_is_restored_whole() {
    let mut f = Fixture::new(cores());
    f.advance();
    f.advance();
    f.advance();
    let saved = f.firmware_zero();
    assert_eq!(
        saved
            .expand(f.history.units())
            .unwrap()
            .iter()
            .filter(|&(_, at)| at != TargetOffset::ZERO)
            .count(),
        3
    );
    f.hold = HOLD_SECS;
    f.qualified.insert(saved.clone());
    f.ceilings.clear();
    let decision = f.decide();
    assert!(matches!(decision.action, ClimbAction::Restore(_)));
    assert_eq!(decision.target(), Some(&saved));
    f.apply(&decision);
    assert_eq!(f.history.current(), &saved);
    assert_eq!(f.history.steps().last().unwrap().changed.len(), 3);
}

#[test]
fn adverse_current_context_evidence_refuses_restoration() {
    let (mut f, saved) = saved_fixture();
    f.hold = HOLD_SECS;
    f.qualified.insert(saved);
    let unit = ids(f.history.units())[0];
    f.ceilings.insert(unit, observed(-8));
    assert!(blocked(&f.decide(), &climb::Blocker::Ceiling { unit }));
}

#[test]
fn firmware_zero_precedes_events_freeze_pause_and_allowance() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.event("pending", cpu());
    f.freeze = Freeze::On;
    f.invocation = Invocation::ColdBoot {
        pause: Pause::Paused,
    };
    f.campaign = Campaign::Unreconstructible;
    f.firmware = Firmware::Changed;
    assert!(matches!(
        f.decide().action,
        ClimbAction::FirmwareZero { .. }
    ));
}

#[test]
fn a_firmware_change_at_stock_does_not_create_a_restoration_back_to_stock() {
    let mut f = Fixture::new(coarse());
    assert!(f.firmware_zero().is_zero());
    f.advance();
    f.hold = HOLD_SECS;
    f.qualified.insert(f.candidate());
    assert!(matches!(f.decide().action, ClimbAction::Promote(_)));
}
