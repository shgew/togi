use super::*;

fn refine(f: &mut Fixture) {
    let inherited = UnitProfile::of_cores(&f.history.current().expand(f.history.units()).unwrap());
    f.history.refine(cores(), inherited).unwrap();
    f.ceilings.clear();
    f.qualified.clear();
}

#[test]
fn sixteen_core_units_inherit_their_ccd_history_without_fictitious_promotions() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.advance();
    let steps = f.history.steps().to_vec();
    let physical = f.history.current().expand(f.history.units()).unwrap();
    refine(&mut f);
    assert_eq!(f.history.units().iter().count(), 16);
    assert_eq!(f.history.steps(), steps);
    assert_eq!(
        f.history.current().expand(f.history.units()).unwrap(),
        physical
    );
    for unit in f.history.units().iter() {
        let parent = unit.parent.unwrap();
        assert!(
            f.history
                .ancestry(unit.id)
                .iter()
                .all(|link| link.transition.source == parent)
        );
        assert_eq!(
            f.history.ancestry(unit.id).len(),
            steps
                .iter()
                .filter(|step| step.changed.contains(&parent))
                .count()
        );
    }
}

#[test]
fn core_units_need_their_own_mapped_edge() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    let ccd_ceilings = f.ceilings.clone();
    refine(&mut f);
    f.ceilings = ccd_ceilings;
    let unit = ids(f.history.units())[0];
    assert!(blocked(&f.decide(), &climb::Blocker::UnknownEdge { unit }));
    f.ceilings.insert(unit, observed(-30));
    let proposal = climb::propose(f.history.current(), &f.ceilings, f.history.finishes()).unwrap();
    assert_eq!(proposal.unit, unit);
    assert_eq!(proposal.from, offset(-5));
}

#[test]
fn a_stopped_ccd_does_not_reopen_through_its_cores() {
    let mut f = Fixture::new(coarse());
    f.advance();
    f.advance();
    f.event("stopped-parent", cpu());
    f.apply_retreat(&[-5, 0]);
    let stopped_parent = ids(f.history.units())[1];
    refine(&mut f);
    for unit in f.history.units().iter() {
        assert_eq!(
            f.history.finishes().is_finished(unit.id),
            unit.parent == Some(stopped_parent)
        );
    }
    for unit in f.history.units().iter() {
        f.ceilings.insert(unit.id, observed(-50));
    }
    let proposal = climb::propose(f.history.current(), &f.ceilings, f.history.finishes()).unwrap();
    assert_ne!(
        f.history.units().get(proposal.unit).unwrap().parent,
        Some(stopped_parent)
    );
    assert_eq!(f.campaign.consumed(), Some(1));
}

#[test]
fn escalation_consumes_shared_parent_transitions_once() {
    let mut f = Fixture::new(coarse());
    for _ in 0..6 {
        f.advance();
    }
    f.event("first", cpu());
    f.apply_retreat(&[-15, -10]);
    refine(&mut f);
    f.event("second", cpu());
    let expected: Vec<_> = f
        .history
        .units()
        .iter()
        .map(|unit| {
            if unit.cores.iter().next().unwrap().get() < 8 {
                -10
            } else {
                -5
            }
        })
        .collect();
    let decision = f.apply_retreat(&expected);
    let ClimbAction::Retreat(retreat) = decision.action else {
        panic!("not a retreat");
    };
    assert_eq!(retreat.cursors.len(), 16);
    let actual_transitions: BTreeSet<_> = retreat
        .cursors
        .iter()
        .map(|cursor| cursor.transition)
        .collect();
    assert_eq!(actual_transitions.len(), 2);
    assert_eq!(f.history.steps().len(), 6);
    f.event("third", cpu());
    let expected: Vec<_> = f
        .history
        .units()
        .iter()
        .map(|unit| {
            if unit.cores.iter().next().unwrap().get() < 8 {
                -5
            } else {
                0
            }
        })
        .collect();
    let third = f.apply_retreat(&expected);
    let ClimbAction::Retreat(retreat) = third.action else {
        panic!("not a retreat");
    };
    assert!(
        retreat
            .cursors
            .iter()
            .all(|cursor| !actual_transitions.contains(&cursor.transition))
    );
    assert_eq!(f.campaign.consumed(), Some(3));
}

#[test]
fn an_application_failure_before_day_applied_consumes_no_step() {
    let mut f = Fixture::new(coarse());
    f.qualified.insert(f.candidate());
    let first = f.decide();
    assert_eq!(f.history.steps().len(), 0);
    assert_eq!(f.decide(), first);
    f.apply(&first);
    assert_eq!(f.history.steps().len(), 1);
    let before = f.history.clone();
    assert_eq!(
        f.history
            .apply(climb::ApplicationId::new(2).unwrap(), now(), &first),
        Err(climb::ClimbError::StaleDecision)
    );
    assert_eq!(f.history, before);
}
