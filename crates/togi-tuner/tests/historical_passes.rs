use std::num::NonZeroU32;
use std::path::Path;

use test_case::test_case;
use togi_journal::Seq;
use togi_machine::{
    Backend, CcdId, Condition, CoreId, CoreInfo, CoreSet, Regime, TargetOffset, workloads,
};
use togi_tuner::{ClassKey, Evidence, Observation, Profile, Secs, SessionRef, Topology};

fn seq(value: u64) -> Seq {
    Seq::new(value).unwrap()
}

fn fixture() -> (Evidence, ClassKey, Profile) {
    let core = CoreId::new(0).unwrap();
    let topology = Topology::new(&[CoreInfo {
        core,
        ccd: CcdId::try_from(0_u8).unwrap(),
        cpus: Vec::new(),
    }])
    .unwrap();
    let class = ClassKey::new(
        workloads(Regime::R6)[0].id(),
        CoreSet::from_iter([core]),
        Secs::new(NonZeroU32::new(120).unwrap()),
    );
    let profile = Profile::new(
        &topology,
        [(core, TargetOffset::new(-10).unwrap())]
            .into_iter()
            .collect(),
    )
    .unwrap();
    (Evidence::new(topology.all()), class, profile)
}

fn observation(seq: u64, class: ClassKey, profile: &Profile) -> Observation<'_> {
    Observation {
        seq: self::seq(seq),
        class,
        profile,
        condition: Condition::Together,
        named: None,
    }
}

#[test_case(1, 0; "pass at boundary is excluded")]
#[test_case(3, 2; "later failure cannot invalidate historical passes")]
#[test_case(4, 2; "failure at boundary is excluded")]
#[test_case(5, 0; "failure before boundary invalidates")]
#[test_case(6, 1; "newer pass counts again")]
fn historical_counts_obey_both_sequence_boundaries(before: u64, expected: usize) {
    let (mut evidence, class, profile) = fixture();
    let backend = evidence.active_backend(Backend::Mprime);
    evidence
        .record_pass(observation(1, class, &profile), backend)
        .unwrap();
    evidence
        .record_carried_pass(
            observation(2, class, &profile),
            backend,
            SessionRef::new("source"),
        )
        .unwrap();
    evidence
        .record_failure(observation(4, class, &profile))
        .unwrap();
    evidence
        .record_pass(observation(5, class, &profile), backend)
        .unwrap();
    assert_eq!(
        evidence
            .passes_before(class, &profile, seq(before))
            .unwrap(),
        expected
    );
}

#[test]
fn backend_identity_applies_to_live_and_carried_passes() {
    let (mut evidence, class, profile) = fixture();
    let old = evidence.active_backend(Backend::Mprime);
    evidence
        .record_pass(observation(1, class, &profile), old)
        .unwrap();
    evidence
        .record_carried_pass(
            observation(2, class, &profile),
            old,
            SessionRef::new("source"),
        )
        .unwrap();
    evidence.set_backend(
        Backend::Mprime,
        Some(Path::new("/nix/store/another-mprime")),
    );
    assert_eq!(evidence.passes_before(class, &profile, seq(3)).unwrap(), 0);
    evidence.set_backend(Backend::Mprime, None);
    assert_eq!(evidence.passes_before(class, &profile, seq(3)).unwrap(), 2);
}

#[test_case(false; "live idle failure")]
#[test_case(true; "carried idle failure")]
fn historical_idle_failures_invalidate_all_core_r6(carried: bool) {
    let (mut evidence, class, profile) = fixture();
    let backend = evidence.active_backend(Backend::Mprime);
    evidence
        .record_pass(observation(1, class, &profile), backend)
        .unwrap();
    if carried {
        evidence
            .record_carried_idle_failure(seq(2), &profile, SessionRef::new("source"))
            .unwrap();
    } else {
        evidence.record_idle_failure(seq(2), &profile).unwrap();
    }
    assert_eq!(evidence.passes_before(class, &profile, seq(2)).unwrap(), 1);
    assert_eq!(evidence.passes_before(class, &profile, seq(3)).unwrap(), 0);
}

#[test]
fn historical_boundaries_search_both_shared_head_and_owned_tail() {
    let (mut evidence, class, profile) = fixture();
    let backend = evidence.active_backend(Backend::Mprime);
    evidence
        .record_pass(observation(1, class, &profile), backend)
        .unwrap();
    let retained = evidence.clone();
    for value in 2..=1030 {
        evidence
            .record_pass(observation(value, class, &profile), backend)
            .unwrap();
    }
    assert_eq!(
        evidence.passes_before(class, &profile, seq(500)).unwrap(),
        499
    );
    assert_eq!(
        evidence.passes_before(class, &profile, seq(1028)).unwrap(),
        1027
    );
    assert_eq!(
        retained.passes_before(class, &profile, seq(1031)).unwrap(),
        1
    );
}
