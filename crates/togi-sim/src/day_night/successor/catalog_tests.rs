use super::*;
use test_case::test_case;

#[test]
fn successor_catalog_is_disjoint_and_stable() {
    assert_eq!(SCENARIO_IDS.len(), 28);
    for (index, id) in SCENARIO_IDS.iter().enumerate() {
        assert!(!SCENARIO_IDS[..index].contains(id), "{id}");
        assert!(!crate::day_night::SCENARIO_IDS.contains(id), "{id}");
        let scenario = Scenario::builtin(id, 20001).unwrap();
        assert_eq!(scenario.id(), *id);
        assert_eq!(
            scenario.topology().len(),
            if *id == "successor-single-ccd" { 8 } else { 16 }
        );
    }
}

#[test_case("successor-firmware-change", 180, 0, 1, EpochKind::Initial)]
#[test_case("successor-firmware-change", 181, 1, 181, EpochKind::Firmware)]
#[test_case("successor-moving-cap", 45, 0, 1, EpochKind::Initial)]
#[test_case("successor-moving-cap", 46, 1, 46, EpochKind::Moving)]
#[test_case("successor-moving-cap", 730, 10, 451, EpochKind::Moving)]
fn epochs_use_public_one_based_days(
    id: &str,
    day: u32,
    index: u32,
    first_day: u32,
    kind: EpochKind,
) {
    let scenario = Scenario::builtin(id, 20001).unwrap();
    assert_eq!(
        scenario.epoch(day).unwrap(),
        TruthEpoch {
            index,
            first_day,
            kind
        }
    );
    assert_eq!(
        scenario.epoch(0),
        Err(TruthError::Invalid("one-based calendar day"))
    );
}

#[test]
fn heterogeneous_curves_follow_seed_permuted_physical_identity() {
    let a = Scenario::builtin("successor-within-ccd-heterogeneous", 20001).unwrap();
    let repeated = Scenario::builtin(a.id(), 20001).unwrap();
    let b = Scenario::builtin(a.id(), 20002).unwrap();
    assert_eq!(a.weak, repeated.weak);
    assert_ne!(a.weak, b.weak);
    assert_eq!(a.weak.len(), 8);
    let class = &a.daily_mix()[0].class;
    for info in a.topology() {
        let rate = a
            .core_rate(class, info.core, Depth::new(21).unwrap(), 1)
            .unwrap();
        assert_eq!(rate > 0.0, a.weak.contains(&info.core));
        assert_eq!(
            a.core_rate(class, info.core, Depth::new(0).unwrap(), 1)
                .unwrap(),
            0.0
        );
    }
}

#[test]
fn changing_one_readback_cannot_be_hidden_by_the_ccd_minimum() {
    let scenario = Scenario::builtin("successor-within-ccd-heterogeneous", 20001).unwrap();
    let base = Profile::uniform(scenario.topology(), Depth::new(20).unwrap());
    let mut changed = *base.depths();
    changed.set(scenario.weak[0], Depth::new(21).unwrap());
    let changed = Profile::new(changed, scenario.topology()).unwrap();
    let state = TruthState::cold(scenario.topology()).unwrap();
    let class = &scenario.daily_mix()[0].class;
    let original = scenario
        .evaluate(class, &base, &state, class.duration(), 1)
        .unwrap();
    let changed = scenario
        .evaluate(class, &changed, &state, class.duration(), 1)
        .unwrap();
    assert_eq!(original.co_total(), 0.0);
    assert!(changed.co_total() > 0.0);
}

#[test]
fn physical_profile_refuses_missing_and_extra_readbacks() {
    let scenario = Scenario::builtin("successor-single-ccd", 20001).unwrap();
    let profile = Profile::uniform(scenario.topology(), Depth::new(20).unwrap());
    assert_eq!(profile.depths().len(), 8);
    assert!(Profile::new(PerCore::default(), scenario.topology()).is_err());
    let mut extra = *profile.depths();
    extra.set(CoreId::new(8).unwrap(), Depth::new(20).unwrap());
    assert!(Profile::new(extra, scenario.topology()).is_err());
    assert_eq!(
        Depth::new(51),
        Err(TruthError::Invalid("physical-core depth"))
    );
}

#[test]
fn hidden_class_is_missing_without_hiding_covered_daily_classes() {
    let scenario = Scenario::builtin("successor-weak-core-hidden", 20001).unwrap();
    let missing = scenario.missing_night_classes();
    assert_eq!(missing.len(), 2);
    for class in missing {
        assert_eq!(
            class.workload(),
            crate::day_night::scenarios::workload(togi_machine::Regime::R2, 2).unwrap()
        );
    }
}

#[test]
fn refinement_preserves_inherited_depth_and_offers_the_full_ceiling_range() {
    let scenario = Scenario::builtin("successor-core-refinement", 20001).unwrap();
    let refinement = scenario.refinement().unwrap().unwrap();
    assert_eq!(refinement.inherited.mean_depth(), 35.0);
    assert_eq!(refinement.candidates.len(), 48);
    for info in scenario.topology() {
        let ceiling = refinement.ceilings.get(info.core).unwrap();
        let edge = refinement.night_edges.get(info.core).unwrap();
        assert_eq!(edge.get() - 5, ceiling.get());
        let class = &scenario.daily_mix()[0].class;
        assert_eq!(
            scenario
                .core_rate(class, info.core, Depth::new(edge.get() - 1).unwrap(), 1)
                .unwrap(),
            0.0
        );
        assert!(scenario.core_rate(class, info.core, edge, 1).unwrap() > 0.0);
        let candidates = refinement
            .candidates
            .iter()
            .filter(|(core, _)| *core == info.core)
            .collect::<Vec<_>>();
        assert_eq!(candidates.len(), usize::from(ceiling.get() - 35));
        assert_eq!(
            candidates.last().unwrap().1.depths().get(info.core),
            Some(ceiling)
        );
        for (_, candidate) in candidates {
            for other in scenario
                .topology()
                .iter()
                .filter(|other| other.core != info.core)
            {
                assert_eq!(
                    candidate.depths().get(other.core),
                    refinement.inherited.depths().get(other.core)
                );
            }
        }
    }
}

#[test]
fn inherited_uniform_sources_preserve_all_original_families() {
    // Exhaustive catalog and integer-depth sweep, not a substitute for per-core fixtures.
    for (old_id, new_id) in crate::day_night::SCENARIO_IDS
        .iter()
        .zip(SCENARIO_IDS.iter())
    {
        let old = OriginalScenario::builtin(old_id, 20001).unwrap();
        let new = Scenario::builtin(new_id, 20001).unwrap();
        let old_state = crate::day_night::TruthState::cold(old.ccd_count()).unwrap();
        let new_state = TruthState::cold(new.topology()).unwrap();
        for depth in 0..=50 {
            let depth = Depth::new(depth).unwrap();
            let old_profile = vec![depth.offset().unwrap(); old.ccd_count()];
            let new_profile = Profile::uniform(new.topology(), depth);
            for (old_block, new_block) in old.daily_mix().iter().zip(new.daily_mix()) {
                let old_exposure = old
                    .evaluate(
                        &old_block.class,
                        &old_profile,
                        &old_state,
                        old_block.class.duration(),
                        0,
                    )
                    .unwrap();
                let new_exposure = new
                    .evaluate(
                        &new_block.class,
                        &new_profile,
                        &new_state,
                        new_block.class.duration(),
                        1,
                    )
                    .unwrap();
                let difference = (old_exposure.co_total() - new_exposure.co_total()).abs();
                assert!(
                    difference <= old_exposure.co_total().max(1.0) * 1e-12,
                    "{old_id}, depth {}",
                    depth.get()
                );
                assert_eq!(old_exposure.background, new_exposure.background, "{old_id}");
            }
        }
    }
}

#[test_case(30, false)]
#[test_case(31, true)]
fn non_cpu_interruptions_use_one_based_public_days(day: u32, event: bool) {
    let scenario = Scenario::builtin("successor-non-cpu-correct", 20001).unwrap();
    let expected = event.then_some((
        crate::day_night::NonCpuCause::Graphics,
        crate::day_night::NonCpuAnswer::NotCpu,
    ));
    assert_eq!(scenario.non_cpu_event(day).unwrap(), expected);
    assert_eq!(
        scenario.non_cpu_event(0),
        Err(TruthError::Invalid("one-based calendar day"))
    );
}
