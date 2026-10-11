use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use serde::Deserialize;
use togi_machine::{CcdId, CoreId, CoreInfo, PerCore, TargetOffset};
use togi_tuner::{CombinationId, Constraints, FailurePoint, Profile, Ranking, Topology};

#[derive(Deserialize)]
struct Fixture {
    failure_points: Vec<(u8, i64)>,
    hi: Vec<i64>,
    ranking: Vec<u8>,
    want: Vec<i64>,
    combinations: Vec<Vec<(u8, i64)>>,
}

#[test]
fn optimum_with_accumulated_combinations() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("data/optimum-accumulated-combinations.json")).unwrap();
    let core = |id: u8| CoreId::new(i64::from(id)).unwrap();
    let offset = |value: i64| TargetOffset::new(value).unwrap();
    let infos: Vec<CoreInfo> = (0..u8::try_from(fixture.hi.len()).unwrap())
        .map(|id| CoreInfo {
            core: core(id),
            ccd: CcdId(id / 8),
            cpus: Vec::new(),
        })
        .collect();
    let topology = Topology::new(&infos).unwrap();
    let profile = |values: &[i64]| {
        let offsets: PerCore<TargetOffset> = topology
            .all()
            .iter()
            .zip(values)
            .map(|(core, &value)| (core, offset(value)))
            .collect();
        Profile::new(&topology, offsets).unwrap()
    };

    let mut constraints = Constraints::new(&topology);
    for &(id, point) in &fixture.failure_points {
        constraints
            .insert_failure_point(core(id), FailurePoint::new(offset(point)))
            .unwrap();
    }
    for (index, combination) in fixture.combinations.iter().enumerate() {
        let id = CombinationId::new(NonZeroU32::new(u32::try_from(index).unwrap() + 1).unwrap());
        let members: PerCore<TargetOffset> = combination
            .iter()
            .map(|&(id, value)| (core(id), offset(value)))
            .collect();
        constraints.insert_combination(id, members).unwrap();
    }
    let ranking = Ranking::new(
        &topology,
        fixture.ranking.iter().map(|&id| core(id)).collect(),
    )
    .unwrap();

    let started = Instant::now();
    let got = constraints.optimum(&profile(&fixture.hi), &ranking);
    let elapsed = started.elapsed();

    assert_eq!(got, Some(profile(&fixture.want)));
    assert!(
        elapsed < Duration::from_secs(10),
        "optimum took {elapsed:?}"
    );
}
