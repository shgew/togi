// Seeded hidden limits and the random streams. Go: the limit drawing of New; stream determinism and
// independence are new (Go keyed streams by formatted strings), and are also pinned in the module's own tests.

use togi_sim::Model;
use togi_sim::random::{Domain, stream};

use crate::common::{core_def, flat_cores, machine};

fn seeded(seed: u64, cores: i64) -> Model {
    Model::new(&machine(cores, vec![]), seed).expect("a valid machine")
}

#[test]
fn the_same_seed_draws_the_same_limits() {
    let first = seeded(7, 16);
    assert_eq!(first.limits(), seeded(7, 16).limits());
    assert_ne!(first.limits(), seeded(8, 16).limits());
    assert_eq!(first.seed(), 7);
}

#[test]
fn a_definition_with_limits_ignores_the_seed() {
    let definition = machine(4, flat_cores(4, -20, -18));
    let one = Model::new(&definition, 1).expect("valid");
    let two = Model::new(&definition, 2).expect("valid");
    assert_eq!(one.limits(), two.limits());
    assert_eq!(one.limits()[0], {
        let core = core_def(0, -20, -18);
        togi_sim::model::Limits {
            alone: core.alone,
            together: core.together,
            idle: None,
            flat: 0.0,
            workload: vec![],
        }
    });
}

#[test]
fn drawn_limits_stay_within_their_documented_bounds() {
    let mut zero_together = 0usize;
    let mut together = 0usize;
    let (mut lowest, mut highest) = (0, -50);
    for seed in 0..300 {
        for limits in seeded(seed, 16).limits() {
            let shallowest = *limits.alone.iter().max().expect("five limits");
            let deepest = *limits.alone.iter().min().expect("five limits");
            assert!(deepest >= -40 && shallowest <= -3, "{limits:?}");
            assert!(
                shallowest - deepest <= 2,
                "alone limits spread at most 2: {limits:?}"
            );
            lowest = lowest.min(deepest);
            highest = highest.max(shallowest);
            for (index, &limit) in limits.together.iter().enumerate() {
                let floor = if index < 5 {
                    limits.alone[index]
                } else {
                    shallowest
                };
                assert!(
                    (floor..=(floor + 3).min(0)).contains(&limit),
                    "{index}: {limits:?}"
                );
                together += 1;
                zero_together += usize::from(limit == floor);
            }
            assert!(limits.idle.is_none() && limits.flat == 0.0 && limits.workload.is_empty());
        }
    }
    assert!(
        lowest <= -38 && highest >= -7,
        "the base spans -40..=-5: {lowest} {highest}"
    );
    #[expect(
        clippy::cast_precision_loss,
        reason = "counts of a few tens of thousands"
    )]
    let share = zero_together as f64 / together as f64;
    assert!(
        (share - 0.75).abs() < 0.02,
        "a together limit equals its alone limit {share} of the time"
    );
}

#[test]
fn r6_and_r7_start_from_the_shallowest_alone_limit() {
    for seed in 0..50 {
        for limits in seeded(seed, 4).limits() {
            let shallowest = *limits.alone.iter().max().expect("five limits");
            assert!(limits.together[5] >= shallowest && limits.together[6] >= shallowest);
        }
    }
}

#[test]
fn a_smaller_machine_draws_only_its_own_cores() {
    for cores in [2, 8] {
        assert_eq!(
            seeded(5, cores).limits().len(),
            usize::try_from(cores).expect("count")
        );
    }
}

#[test]
fn streams_of_one_seed_do_not_share_state() {
    use rand::Rng;
    let key = [3, 1];
    let mut alone = stream(9, Domain::Core, &key);
    let expected: Vec<u64> = (0..8).map(|_| alone.next_u64()).collect();
    let mut telemetry = stream(9, Domain::Signal, &key);
    let mut core = stream(9, Domain::Core, &key);
    let mut interleaved = Vec::new();
    for _ in 0..8 {
        let _ = telemetry.next_u64();
        interleaved.push(core.next_u64());
    }
    assert_eq!(interleaved, expected, "drawing elsewhere changes nothing");
}
