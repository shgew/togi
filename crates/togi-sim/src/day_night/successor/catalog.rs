use rand::seq::SliceRandom;
use togi_machine::{Condition, PerCore, Regime, TrialRole};

use super::{Fixture, OriginalScenario, Scenario};
use crate::day_night::{DailyBlock, TrialClass, TruthError};
use crate::random::{self, Domain};

/// Stable order for the successor registration, disjoint from the frozen original IDs.
pub const SCENARIO_IDS: [&str; 28] = [
    "successor-edge-shallow",
    "successor-edge-deep",
    "successor-no-write-limit-failure",
    "successor-slope-flat",
    "successor-slope-steep",
    "successor-flattening-tail",
    "successor-flatter-dangerous-class",
    "successor-delayed-onset",
    "successor-thermal-history",
    "successor-hidden-daily",
    "successor-partial-only",
    "successor-single-core-binding",
    "successor-jointly-binding",
    "successor-r8-nonbinding",
    "successor-both-ccd-interaction",
    "successor-stock-background",
    "successor-single-ccd",
    "successor-moving-cap",
    "successor-firmware-change",
    "successor-non-cpu-correct",
    "successor-non-cpu-wrong",
    "successor-non-cpu-missing",
    "successor-within-ccd-heterogeneous",
    "successor-weak-core-hidden",
    "successor-weak-core-delayed",
    "successor-combined-adversary",
    "successor-core-interactions",
    "successor-core-refinement",
];

pub(super) fn builtin(name: &str, seed: u64) -> Result<Scenario, TruthError> {
    let id = SCENARIO_IDS
        .iter()
        .copied()
        .find(|id| *id == name)
        .ok_or_else(|| TruthError::UnknownScenario(name.to_owned()))?;
    let (fixture, base) = match id {
        "successor-within-ccd-heterogeneous" => (Fixture::Heterogeneous, "edge-deep"),
        "successor-weak-core-hidden" => (Fixture::Hidden, "edge-deep"),
        "successor-weak-core-delayed" => (Fixture::Delayed, "edge-deep"),
        "successor-combined-adversary" => (Fixture::Combined, "edge-deep"),
        "successor-core-interactions" => (Fixture::Interactions, "edge-deep"),
        "successor-core-refinement" => (Fixture::Refinement, "edge-deep"),
        _ => (
            Fixture::Legacy,
            id.strip_prefix("successor-")
                .ok_or(TruthError::Invalid("successor scenario namespace"))?,
        ),
    };
    let original = OriginalScenario::builtin(base, seed)?;
    let mut rng = random::stream(seed, Domain::Limits, &[0x5355_4343_4553_534f]);
    let mut weak = Vec::new();
    for ccd in original
        .topology()
        .iter()
        .map(|info| info.ccd)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let mut cores = original
            .topology()
            .iter()
            .filter(|info| info.ccd == ccd)
            .map(|info| info.core)
            .collect::<Vec<_>>();
        cores.shuffle(&mut rng);
        let count = match fixture {
            Fixture::Heterogeneous | Fixture::Refinement => cores.len() / 2,
            Fixture::Hidden | Fixture::Delayed | Fixture::Combined => 1,
            Fixture::Legacy | Fixture::Interactions => 0,
        };
        weak.extend(cores.into_iter().take(count));
    }
    let mut daily = original.daily_mix().to_vec();
    if matches!(fixture, Fixture::Hidden | Fixture::Combined) {
        // The hidden workload is absent at night, with one seed-permuted weak core per CCD.
        for block in daily.iter_mut().skip(2) {
            let mut spec = block
                .class
                .to_spec(String::new(), PerCore::default(), 0, 0)?;
            spec.regime = Regime::R2;
            spec.workload = crate::day_night::scenarios::workload(Regime::R2, 2)?;
            let target = original
                .topology()
                .iter()
                .find(|info| weak.contains(&info.core) && spec.cores.contains(&info.core))
                .ok_or(TruthError::Invalid("hidden weak-core placement"))?;
            spec.cores = vec![target.core];
            spec.cpus = vec![*target.cpus.first().ok_or(TruthError::Identity)?];
            spec.roles = TrialRole::legacy(
                spec.regime,
                spec.workload,
                0,
                &spec.cores,
                &spec.cpus,
                original.topology(),
            )
            .map_err(TruthError::Roles)?;
            spec.condition = Condition::Alone;
            block.class = TrialClass::from_spec(&spec)?;
        }
    }
    let nights = if fixture == Fixture::Legacy {
        original.night_classes().to_vec()
    } else {
        let mut nights = original
            .night_classes()
            .iter()
            .filter(|class| class.duration().as_secs() <= 300)
            .cloned()
            .collect::<Vec<_>>();
        if !matches!(
            fixture,
            Fixture::Hidden | Fixture::Delayed | Fixture::Combined
        ) {
            for DailyBlock { class, .. } in &daily {
                if !nights.contains(class) {
                    nights.push(class.clone());
                }
            }
        }
        if fixture == Fixture::Hidden {
            for block in daily.iter().take(2) {
                if !nights.contains(&block.class) {
                    nights.push(block.class.clone());
                }
            }
        }
        nights
    };
    let topology = original.topology();
    let interactions = if matches!(fixture, Fixture::Interactions | Fixture::Combined) {
        vec![
            (topology[0].core, topology[1].core),
            (topology[0].core, topology[8].core),
        ]
    } else if fixture == Fixture::Legacy && original.family == crate::day_night::Family::Interaction
    {
        vec![(topology[0].core, topology[8].core)]
    } else {
        Vec::new()
    };
    Ok(Scenario {
        id,
        original,
        fixture,
        weak,
        daily,
        nights,
        interactions,
    })
}
