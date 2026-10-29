use test_case::test_case;
use togi_journal::Seq;
use togi_tuner::climb::{Blocker, FinishBook};
use togi_tuner::day::{DayInterval, IntervalKind, Readback, credited_secs};

use super::*;

#[test]
fn the_first_step_needs_fifty_clean_stock_hours() {
    let mut f = Fixture::new(coarse());
    f.qualified.insert(f.candidate());
    f.hold = HOLD_SECS - 60;
    assert!(blocked(
        &f.decide(),
        &Blocker::Hold {
            credited_secs: HOLD_SECS - 60,
            required_secs: HOLD_SECS
        }
    ));
    f.hold = HOLD_SECS;
    assert!(matches!(f.decide().action, ClimbAction::Promote(_)));
}

#[test]
fn paused_suspended_and_unobserved_time_never_counts_toward_a_hold() {
    let units = coarse();
    let stock = UnitProfile::zero(&units);
    let intervals = [
        IntervalKind::Paused,
        IntervalKind::Suspended,
        IntervalKind::Unobserved,
        IntervalKind::Ambiguous,
        IntervalKind::Applied {
            profile: stock.clone(),
            readback: Readback::Unverified,
        },
        IntervalKind::Applied {
            profile: stock.clone(),
            readback: Readback::Verified,
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, kind)| DayInterval {
        at: Seq::new(u64::try_from(index).unwrap() + 2).unwrap(),
        secs: 3600,
        kind,
    })
    .collect::<Vec<_>>();
    assert_eq!(
        credited_secs(&intervals, &stock, Seq::new(1).unwrap()),
        3600
    );
}

#[test]
fn the_step_is_half_the_distance_to_the_ceiling_rounded_up_and_at_most_five() {
    let units = coarse();
    let unit = ids(&units)[0];
    // Exhaustively exercise the complete signed-offset distance domain, including r=50.
    for r in 1..=50 {
        let mut ceilings = Ceilings::new();
        // The arithmetic helper also supports externally validated ceilings across the write range.
        ceilings.insert(
            unit,
            Ceiling {
                edge: EdgeState::Unknown,
                c: Some(offset(-r)),
            },
        );
        let proposal = climb::propose(
            &UnitProfile::zero(&units),
            &ceilings,
            &FinishBook::default(),
        )
        .unwrap();
        assert_eq!(proposal.step, ((r + 1) / 2).clamp(1, 5), "r={r}");
        assert_eq!(proposal.to, offset(-proposal.step), "r={r}");
        assert!(proposal.to >= offset(-r), "r={r}");
    }
}

#[test]
fn a_ceiling_of_minus_25_climbs_in_seven_steps() {
    let units = coarse();
    let unit = ids(&units)[0];
    let mut applied = UnitProfile::zero(&units);
    let ceilings = Ceilings::from([(unit, observed(-30))]);
    let mut actual = vec![0];
    while let Some(proposal) = climb::propose(&applied, &ceilings, &FinishBook::default()) {
        actual.push(proposal.to.get());
        applied = proposal.profile;
    }
    assert_eq!(actual, vec![0, -5, -10, -15, -20, -23, -24, -25]);
}

#[test]
fn the_unit_farthest_from_its_ceiling_moves_and_ties_go_to_the_lowest_unit() {
    let mut f = Fixture::new(coarse());
    let units = ids(f.history.units());
    let tied = climb::propose(f.history.current(), &f.ceilings, f.history.finishes()).unwrap();
    assert_eq!(tied.unit, units[0]);
    f.ceilings.insert(units[1], observed(-40));
    assert_eq!(
        climb::propose(f.history.current(), &f.ceilings, f.history.finishes())
            .unwrap()
            .unit,
        units[1]
    );
}

#[test]
fn exactly_one_unit_changes_per_step() {
    let f = Fixture::new(cores());
    let proposal = climb::propose(f.history.current(), &f.ceilings, f.history.finishes()).unwrap();
    assert_eq!(
        proposal
            .profile
            .iter()
            .filter(|&(unit, depth)| f.history.current().get(unit) != Some(depth))
            .count(),
        1
    );
}

#[test]
fn a_certificate_for_another_vector_never_qualifies_the_proposal() {
    let mut f = Fixture::new(coarse());
    f.qualified.insert(profile(f.history.units(), &[-5, -5]));
    assert!(blocked(&f.decide(), &Blocker::Certificate));
}

#[test]
fn an_unknown_edge_gives_its_unit_no_promotion() {
    let mut f = Fixture::new(coarse());
    let units = ids(f.history.units());
    f.ceilings
        .insert(units[0], Ceiling::new(EdgeState::Unknown));
    assert_eq!(
        climb::propose(f.history.current(), &f.ceilings, f.history.finishes())
            .unwrap()
            .unit,
        units[1]
    );
    f.ceilings
        .insert(units[1], Ceiling::new(EdgeState::Unknown));
    assert!(blocked(
        &f.decide(),
        &Blocker::UnknownEdge { unit: units[0] }
    ));
}

#[test_case(-30, -25; "deep")]
#[test_case(-4, 0; "stock_margin")]
fn an_observed_edge_sets_the_ceiling_five_counts_shallower(edge: i64, ceiling: i64) {
    assert_eq!(observed(edge).c, Some(offset(ceiling)));
}

#[test]
fn a_censored_write_limit_edge_caps_the_ceiling_at_minus_45() {
    let ceiling = Ceiling::new(EdgeState::Censored {
        witnesses: vec![evidence()],
    });
    assert_eq!(ceiling.c, Some(offset(-45)));
    assert!(matches!(ceiling.edge, EdgeState::Censored { .. }));
}

#[test]
fn a_shallower_edge_retreats_below_the_new_ceiling_and_stops_the_unit() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.advance();
    let unit = ids(f.history.units())[0];
    f.ceilings.insert(unit, observed(-8));
    f.apply_retreat(&[-3, -5]);
    assert!(matches!(
        f.history.finishes().get(unit).unwrap().reason,
        climb::FinishReason::Stopped(climb::StopReason::AdverseCeiling)
    ));
}

#[test]
fn an_unqualified_safety_target_falls_back_to_stock() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.advance();
    f.ceilings.insert(ids(f.history.units())[0], observed(-8));
    f.qualified.clear();
    let decision = f.decide();
    let ClimbAction::Retreat(retreat) = &decision.action else {
        panic!("{decision:?}");
    };
    assert!(retreat.profile.is_zero());
    assert_eq!(retreat.wanted, profile(f.history.units(), &[-3, -5]));
    f.apply(&decision);
    f.qualified.insert(profile(f.history.units(), &[-3, -5]));
    assert_eq!(
        f.decide().target(),
        Some(&profile(f.history.units(), &[-3, -5]))
    );
}

#[test]
fn a_zero_statistical_cap_neither_blocks_nor_retreats_a_qualified_step() {
    // Statistical projections are deliberately absent from the permission interface.
    let mut f = Fixture::new(coarse());
    f.qualified.insert(f.candidate());
    let decision = f.decide();
    assert_eq!(
        decision.target(),
        Some(&profile(f.history.units(), &[-5, 0]))
    );
    assert!(matches!(decision.action, ClimbAction::Promote(_)));
    f.apply(&decision);
    assert!(matches!(f.decide().action, ClimbAction::Hold { .. }));
}

#[test]
fn a_failed_candidate_certificate_blocks_that_vector_without_charging_the_allowance() {
    let mut f = Fixture::new(coarse());
    let candidate = f.candidate();
    f.failed.insert(candidate.clone());
    f.qualified.insert(candidate);
    assert!(blocked(&f.decide(), &Blocker::FailedCertificate));
    assert_eq!(f.campaign.consumed(), Some(0));
    assert_eq!(f.history.steps().len(), 0);
}

#[test]
fn a_unit_at_its_ceiling_stays_finished_after_a_deeper_edge() {
    let mut f = Fixture::new(coarse());
    let units = ids(f.history.units());
    f.ceilings.insert(units[0], observed(-6));
    f.ceilings.insert(units[1], observed(-5));
    f.advance();
    assert!(f.history.finishes().is_finished(units[0]));
    f.ceilings.insert(units[0], observed(-40));
    assert_eq!(
        climb::propose(f.history.current(), &f.ceilings, f.history.finishes()),
        None
    );
}

#[derive(Clone, Copy)]
enum Prerequisite {
    Freeze,
    Pause,
    SameBoot,
    Pending,
    Unresolved,
    Firmware,
    Activation,
    SafeApply,
}

#[test_case(Prerequisite::Freeze; "freeze")]
#[test_case(Prerequisite::Pause; "pause")]
#[test_case(Prerequisite::SameBoot; "same_boot")]
#[test_case(Prerequisite::Pending; "pending")]
#[test_case(Prerequisite::Unresolved; "unresolved")]
#[test_case(Prerequisite::Firmware; "firmware")]
#[test_case(Prerequisite::Activation; "activation")]
#[test_case(Prerequisite::SafeApply; "safe_apply")]
fn every_safe_apply_prerequisite_blocks_promotion_but_not_mandatory_safety(which: Prerequisite) {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.hold = HOLD_SECS;
    f.qualified.insert(f.candidate());
    let blocker = match which {
        Prerequisite::Freeze => {
            f.freeze = Freeze::On;
            Blocker::Freeze
        }
        Prerequisite::Pause => {
            f.invocation = Invocation::ColdBoot {
                pause: Pause::Paused,
            };
            Blocker::Pause
        }
        Prerequisite::SameBoot => {
            f.invocation = Invocation::SameBoot;
            Blocker::SameBoot
        }
        Prerequisite::Pending => {
            f.pending_question = true;
            Blocker::PendingQuestion
        }
        Prerequisite::Unresolved => {
            f.unresolved_event = true;
            Blocker::UnresolvedEvent
        }
        Prerequisite::Firmware => {
            f.apply.firmware_valid = false;
            Blocker::FirmwareInvalid
        }
        Prerequisite::Activation => {
            f.apply.activation_open = false;
            Blocker::ActivationClosed
        }
        Prerequisite::SafeApply => {
            f.apply.safe_apply = false;
            Blocker::SafeApply
        }
    };
    assert!(blocked(&f.decide(), &blocker));
    f.event("safety", cpu());
    assert!(matches!(f.decide().action, ClimbAction::Retreat(_)));
}

#[test]
fn a_single_unit_obeys_the_same_hold_and_step_rules() {
    let topology = Topology::new(&[CoreInfo {
        core: CoreId::new(0).unwrap(),
        ccd: CcdId::new(0).unwrap(),
        cpus: Vec::new(),
    }])
    .unwrap();
    let mut f = Fixture::new(Units::coarse(&topology));
    f.advance();
    assert_eq!(f.history.current().iter().count(), 1);
    assert_eq!(f.history.current().iter().next().unwrap().1, offset(-5));
    assert!(blocked(
        &f.decide(),
        &Blocker::Hold {
            credited_secs: 0,
            required_secs: HOLD_SECS
        }
    ));
}

#[test]
fn a_transient_hold_is_not_a_finish() {
    let f = Fixture::new(coarse());
    let unit = ids(f.history.units())[0];
    assert_eq!(
        f.history.finishes().state(unit, Some(Blocker::Certificate)),
        climb::UnitState::Held {
            reason: Blocker::Certificate
        }
    );
    assert!(!f.history.finishes().is_finished(unit));
    assert_eq!(
        f.history.finishes().state(unit, None),
        climb::UnitState::Climbing
    );
}
