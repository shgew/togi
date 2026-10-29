use togi_machine::{ObservedOffset, PerCore};
use togi_tuner::day::{CoreProfile, ProvisionalCause, Witness, classify};

use super::*;

#[test]
fn a_current_profile_requalification_failure_is_a_cpu_suspect_event() {
    let mut f = Fixture::new(coarse());
    f.advance();
    let actual = f.history.actual(f.history.current()).unwrap();
    let class = classify(&actual, Witness::FailedRequalification, &[], None);
    assert_eq!(
        class,
        Classification::CpuSuspect(CpuCause::FailedRequalification)
    );
    f.event("requalification", class);
    assert!(matches!(f.decide().action, ClimbAction::Retreat(_)));
    assert_eq!(f.campaign.consumed(), Some(1));
}

#[test]
fn a_whole_profile_veto_forbids_every_deeper_per_core_vector() {
    let units = cores();
    let veto = Saved::new(
        &units,
        &[
            -5, -10, -15, -20, -5, -10, -15, -20, -5, -10, -15, -20, -5, -10, -15, -20,
        ],
    );
    assert!(veto.0.forbids(&veto.0));
    let deeper = CoreProfile::new(
        veto.0
            .offsets()
            .iter()
            .map(|(core, at)| (core, ObservedOffset(at.0 - 1)))
            .collect(),
    )
    .unwrap();
    assert!(veto.0.forbids(&deeper));
    let mut incomparable: PerCore<_> = *deeper.offsets();
    let first = CoreId::new(0).unwrap();
    incomparable.set(first, ObservedOffset(-4));
    assert!(!veto.0.forbids(&CoreProfile::new(incomparable).unwrap()));
}

struct Saved(CoreProfile);
impl Saved {
    fn new(units: &Units, values: &[i64]) -> Self {
        Self(
            climb::SavedProfile {
                units: units.clone(),
                profile: profile(units, values),
            }
            .actual()
            .unwrap(),
        )
    }
}

#[test]
fn the_first_backstep_stops_its_unit_permanently() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    let units = ids(f.history.units());
    f.event("first", cpu());
    f.apply_retreat(&[-5, 0]);
    assert!(matches!(
        f.history.finishes().get(units[1]).unwrap().reason,
        climb::FinishReason::Stopped(_)
    ));
    f.ceilings.insert(units[1], observed(-50));
    let proposal = climb::propose(f.history.current(), &f.ceilings, f.history.finishes()).unwrap();
    assert_eq!(proposal.unit, units[0]);
}

#[test]
fn second_and_third_events_walk_every_units_history_to_stock() {
    let mut f = Fixture::new(coarse());
    for _ in 0..4 {
        f.advance();
    }
    f.event("first", cpu());
    f.apply_retreat(&[-10, -5]);
    f.event("second", cpu());
    f.apply_retreat(&[-5, 0]);
    f.event("third", cpu());
    f.apply_retreat(&[0, 0]);
    assert_eq!(f.campaign.consumed(), Some(3));
    assert_eq!(f.campaign.remaining(), Some(0));
    assert_eq!(f.history.consumed().len(), 4);
}

#[test]
fn event_identity_is_unique_across_provisional_manual_and_imported_witnesses() {
    let mut f = Fixture::new(coarse());
    f.advance();
    let canonical = f.event(
        "boot",
        Classification::ProvisionalCpuSuspect(ProvisionalCause::UnansweredUnknown),
    );
    let mut copied = f.events[0].clone();
    copied.id = EventId::Unobserved(now());
    copied.class = cpu();
    f.campaign.observe(
        &copied,
        std::slice::from_ref(&canonical),
        EventPhase::Learning,
        f.history.units().phase(),
    );
    f.campaign.observe(
        &copied,
        std::slice::from_ref(&canonical),
        EventPhase::PostFinish,
        f.history.units().phase(),
    );
    assert_eq!(f.campaign.consumed(), Some(1));
    assert_eq!(f.campaign.charges()[0].aliases.len(), 2);
    assert!(f.campaign.charges()[0].provisional);
}

#[test]
fn a_reclassified_event_keeps_its_charge_and_stop_and_restores_depth_only_under_guard() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    let unit = ids(f.history.units())[1];
    let original = f.history.current().clone();
    f.event(
        "provisional",
        Classification::ProvisionalCpuSuspect(ProvisionalCause::UnsureAnswer),
    );
    f.apply_retreat(&[-5, 0]);
    f.events[0].class = Classification::NonCpu;
    f.campaign.observe(
        &f.events[0],
        &[],
        EventPhase::Learning,
        f.history.units().phase(),
    );
    assert_eq!(f.campaign.consumed(), Some(1));
    assert_eq!(f.campaign.reclassified(), 1);
    assert!(blocked(
        &f.decide(),
        &climb::Blocker::Hold {
            credited_secs: 0,
            required_secs: HOLD_SECS
        }
    ));
    f.hold = HOLD_SECS;
    f.qualified.clear();
    assert!(blocked(&f.decide(), &climb::Blocker::Certificate));
    f.qualified.insert(original.clone());
    f.ceilings.insert(unit, observed(-7));
    assert!(blocked(&f.decide(), &climb::Blocker::Ceiling { unit }));
    f.ceilings.insert(unit, observed(-30));
    f.freeze = Freeze::On;
    assert!(blocked(&f.decide(), &climb::Blocker::Freeze));
    f.freeze = Freeze::Off;
    let mut independent = f.events[0].clone();
    independent.id = EventId::Boot(BootId::from("independent-veto".to_owned()));
    independent.class = cpu();
    f.events.push(independent);
    assert!(blocked(&f.decide(), &climb::Blocker::Veto));
    f.events.pop();
    let decision = f.decide();
    assert!(matches!(
        decision.action,
        ClimbAction::Restore(climb::RestorationProposal {
            kind: climb::RestorationKind::Reclassified { .. },
            ..
        })
    ));
    assert_eq!(decision.target(), Some(&original));
    f.apply(&decision);
    assert!(f.history.finishes().is_finished(unit));
    assert_eq!(f.campaign.consumed(), Some(1));
    assert!(!matches!(f.decide().action, ClimbAction::Restore(_)));
}

#[test]
fn two_learning_events_exhaust_the_machine_allowance() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.event("first", cpu());
    f.apply_retreat(&[-5, 0]);
    f.event("second", cpu());
    f.apply_retreat(&[0, 0]);
    assert_eq!(f.campaign.consumed(), Some(2));
    assert!(f.campaign.exhausted());
    assert!(blocked(&f.decide(), &climb::Blocker::AllowanceExhausted));
    let current = f.history.current().clone();
    let ceilings = f.ceilings.clone();
    f.history
        .finishes_mut()
        .observe(now(), &current, &ceilings, true);
    assert!(
        f.history
            .current()
            .iter()
            .all(|(unit, _)| f.history.finishes().is_finished(unit))
    );
}

#[test]
fn the_allowance_never_renews_across_restart_pause_phase_reset_or_firmware() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.event("charged", cpu());
    f.apply_retreat(&[-5, 0]);
    let charged = f.campaign.clone();
    f.invocation = Invocation::SameBoot;
    let _ = f.decide();
    f.invocation = Invocation::ColdBoot {
        pause: Pause::Paused,
    };
    let _ = f.decide();
    let core_profile =
        UnitProfile::of_cores(&f.history.current().expand(f.history.units()).unwrap());
    f.history.refine(cores(), core_profile).unwrap();
    f.firmware_zero();
    // A session reset replaces successful-profile history, not the campaign ledger.
    f.history = ClimbHistory::stock(cores());
    assert_eq!(f.campaign, charged);
    assert_eq!(f.campaign.remaining(), Some(1));
}

#[test]
fn post_finish_and_third_events_remain_visible_and_stock_events_do_not_charge() {
    let mut f = Fixture::new(coarse());
    f.event("stock", cpu());
    assert_eq!(f.campaign.consumed(), Some(0));
    f.events.clear();
    f.advance();
    f.event("one", cpu());
    let mut event = f.events[0].clone();
    event.id = EventId::Boot(BootId::from("post-finish".to_owned()));
    f.campaign.observe(
        &event,
        &[],
        EventPhase::PostFinish,
        f.history.units().phase(),
    );
    event.id = EventId::Boot(BootId::from("third".to_owned()));
    f.campaign.observe(
        &event,
        &[],
        EventPhase::PostFinish,
        f.history.units().phase(),
    );
    assert_eq!(f.campaign.consumed(), Some(3));
    assert_eq!(
        f.campaign
            .charges()
            .iter()
            .filter(|charge| charge.phase == EventPhase::PostFinish)
            .count(),
        2
    );
}

#[test]
fn unreconstructible_campaign_history_blocks_nonzero_exploration() {
    let mut f = Fixture::new(coarse());
    f.campaign = Campaign::Unreconstructible;
    f.qualified.insert(f.candidate());
    assert!(blocked(&f.decide(), &climb::Blocker::CampaignHistory));
}

#[test]
fn reclassification_restoration_preserves_another_units_later_progress() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.event("later-reclassified", cpu());
    f.apply_retreat(&[-5, 0]);
    f.advance();
    f.events[0].class = Classification::NonCpu;
    f.campaign.observe(
        &f.events[0],
        &[],
        EventPhase::Learning,
        f.history.units().phase(),
    );
    f.hold = HOLD_SECS;
    let target = profile(f.history.units(), &[-10, -5]);
    f.qualified.insert(target.clone());
    let decision = f.decide();
    assert!(matches!(decision.action, ClimbAction::Restore(_)));
    assert_eq!(decision.target(), Some(&target));
}
