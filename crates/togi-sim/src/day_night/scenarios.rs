use std::time::Duration;

use rand::RngExt;
use togi_machine::{
    CcdId, Condition, CoreId, CoreInfo, CpuId, PerCore, Regime, TrialRole, TrialSpec, WorkloadId,
};

use super::{DailyBlock, FailureCurve, Family, REFERENCE_LAMBDA, Scenario, TrialClass, TruthError};
use crate::random::{self, Domain};

/// Frozen scenario names used by the day/night registration.
pub const SCENARIO_IDS: [&str; 22] = [
    "edge-shallow",
    "edge-deep",
    "no-write-limit-failure",
    "slope-flat",
    "slope-steep",
    "flattening-tail",
    "flatter-dangerous-class",
    "delayed-onset",
    "thermal-history",
    "hidden-daily",
    "partial-only",
    "single-core-binding",
    "jointly-binding",
    "r8-nonbinding",
    "both-ccd-interaction",
    "stock-background",
    "single-ccd",
    "moving-cap",
    "firmware-change",
    "non-cpu-correct",
    "non-cpu-wrong",
    "non-cpu-missing",
];

pub(super) fn workload(regime: Regime, index: usize) -> Result<WorkloadId, TruthError> {
    WorkloadId::new(regime, index).ok_or(TruthError::Identity)
}

pub(super) fn builtin(name: &str, seed: u64) -> Result<Scenario, TruthError> {
    let id = SCENARIO_IDS
        .iter()
        .copied()
        .find(|id| *id == name)
        .ok_or_else(|| TruthError::UnknownScenario(name.to_owned()))?;
    let (family, edge, growth, tail, ccd_count, background) = match id {
        "edge-shallow" => (Family::Ordinary, 5.0, 2.0, None, 2, 0.0),
        "edge-deep" => (Family::Ordinary, 48.0, 2.0, None, 2, 0.0),
        "no-write-limit-failure" => (Family::WriteLimitSafe, 70.0, 2.0, None, 2, 0.0),
        "slope-flat" => (Family::Ordinary, 22.0, 1.2, None, 2, 0.0),
        "slope-steep" => (Family::Ordinary, 22.0, 6.0, None, 2, 0.0),
        "flattening-tail" => (Family::Ordinary, 22.0, 3.0, Some(1.2), 2, 0.0),
        "flatter-dangerous-class" => (Family::FlatterDangerous, 22.0, 3.0, None, 2, 0.0),
        "delayed-onset" => (Family::Delayed, 20.0, 2.0, None, 2, 0.0),
        "thermal-history" => (Family::Thermal, 20.0, 2.0, None, 2, 0.0),
        "hidden-daily" => (Family::Hidden, 22.0, 2.0, None, 2, 0.0),
        "partial-only" => (Family::Partial, 22.0, 2.0, None, 2, 0.0),
        "single-core-binding" => (Family::SingleCore, 22.0, 2.0, None, 2, 0.0),
        "jointly-binding" => (Family::Joint, 22.0, 2.0, None, 2, 0.0),
        "r8-nonbinding" => (Family::R8Nonbinding, 22.0, 2.0, None, 2, 0.0),
        "both-ccd-interaction" => (Family::Interaction, 22.0, 2.0, None, 2, 0.0),
        "stock-background" => (Family::Ordinary, 22.0, 2.0, None, 2, REFERENCE_LAMBDA),
        "single-ccd" => (Family::Ordinary, 22.0, 2.0, None, 1, 0.0),
        "moving-cap" => (Family::Moving, 24.0, 2.0, None, 2, 0.0),
        "firmware-change" => (Family::Firmware, 24.0, 2.0, None, 2, 0.0),
        "non-cpu-correct" | "non-cpu-wrong" | "non-cpu-missing" => {
            (Family::Ordinary, 22.0, 2.0, None, 2, 0.0)
        }
        _ => return Err(TruthError::UnknownScenario(name.to_owned())),
    };
    let mut rng = random::stream(seed, Domain::Limits, &[0x4441_594e_4947_4854]);
    // Registered seeds describe different chips, independently of all failure clocks.
    let jitter = if id == "edge-shallow" {
        rng.random_range(0_i32..=2)
    } else {
        rng.random_range(-1_i32..=1)
    };
    let edge = edge + f64::from(jitter);
    FailureCurve::new(edge, growth, tail, REFERENCE_LAMBDA)?;
    let topology = topology(ccd_count)?;
    let mut nights = Vec::new();
    let night_duration = Duration::from_mins(5);
    for info in &topology {
        nights.push(class(
            Regime::R1,
            0,
            &[info.core],
            &topology,
            night_duration,
        )?);
    }
    for ccd in 0..ccd_count {
        let cores = topology
            .iter()
            .filter(|info| usize::from(info.ccd.get()) == ccd)
            .map(|info| info.core)
            .collect::<Vec<_>>();
        nights.push(class(Regime::R7, 0, &cores, &topology, night_duration)?);
        if family == Family::Partial {
            nights.push(class(
                Regime::R7,
                0,
                &cores[..4],
                &topology,
                night_duration,
            )?);
        }
        nights.push(class(
            Regime::R8,
            0,
            &cores[..1],
            &topology,
            night_duration,
        )?);
        nights.push(class(
            Regime::R2,
            0,
            &cores[..1],
            &topology,
            night_duration,
        )?);
    }
    let all = topology.iter().map(|info| info.core).collect::<Vec<_>>();
    nights.push(class(Regime::R7, 0, &all, &topology, night_duration)?);
    nights.push(class(Regime::R9, 0, &all[..1], &topology, night_duration)?);
    let duration = Duration::from_hours(2);
    let sustained = class(Regime::R7, 0, &all, &topology, duration)?;
    let mut daily_classes = vec![sustained.clone(), sustained];
    // Four sustained two-hour conditions. Single-CCD-only conditions remain in the mix
    // beside a simultaneous-CCD condition, so the interaction is not equivalent to a CCD curve.
    for ccd in 0..ccd_count {
        let cores = topology
            .iter()
            .filter(|info| usize::from(info.ccd.get()) == ccd)
            .map(|info| info.core)
            .collect::<Vec<_>>();
        let (regime, index) = match family {
            Family::Hidden => (Regime::R2, 2),
            Family::FlatterDangerous => (Regime::R2, 0),
            Family::Partial => (Regime::R7, 0),
            Family::SingleCore => (Regime::R1, 0),
            Family::Ordinary
            | Family::WriteLimitSafe
            | Family::Delayed
            | Family::Thermal
            | Family::Joint
            | Family::R8Nonbinding
            | Family::Interaction
            | Family::Moving
            | Family::Firmware => (Regime::R7, 0),
        };
        let loaded = if family == Family::Partial {
            &cores[..4]
        } else if matches!(regime, Regime::R1 | Regime::R2 | Regime::R8) {
            &cores[..1]
        } else {
            &cores
        };
        daily_classes.push(class(regime, index, loaded, &topology, duration)?);
    }
    if ccd_count == 1 {
        daily_classes.push(daily_classes[2].clone());
    }
    let classes: [TrialClass; 4] = daily_classes
        .try_into()
        .map_err(|_| TruthError::Invalid("daily block count"))?;
    let daily = classes.map(|class| DailyBlock {
        class,
        fraction: 0.25,
    });
    let hidden = workload(Regime::R2, 2)?;
    for block in &daily {
        if family == Family::Hidden && block.class.workload() == hidden {
            continue;
        }
        if !nights.contains(&block.class) {
            nights.push(block.class.clone());
        }
    }
    let scenario = Scenario {
        id,
        family,
        topology,
        ccd_count,
        edge,
        growth,
        tail,
        background,
        daily,
        nights,
    };
    // Validate all materialized class/CCD curves, including the final moving-cap epoch.
    for block in &scenario.daily {
        for ccd in 0..ccd_count {
            scenario.curve(&block.class, ccd, 730)?;
        }
    }
    Ok(scenario)
}

fn topology(ccd_count: usize) -> Result<Vec<CoreInfo>, TruthError> {
    (0..ccd_count * 8)
        .map(|index| {
            let id = i64::try_from(index).map_err(|_| TruthError::Identity)?;
            let core_count = i64::try_from(ccd_count * 8).map_err(|_| TruthError::Identity)?;
            Ok(CoreInfo {
                core: CoreId::new(id).map_err(|_| TruthError::Identity)?,
                ccd: CcdId::new(id / 8).map_err(|_| TruthError::Identity)?,
                cpus: vec![
                    CpuId::new(id).map_err(|_| TruthError::Identity)?,
                    CpuId::new(id + core_count).map_err(|_| TruthError::Identity)?,
                ],
            })
        })
        .collect()
}

fn class(
    regime: Regime,
    index: usize,
    cores: &[CoreId],
    topology: &[CoreInfo],
    duration: Duration,
) -> Result<TrialClass, TruthError> {
    let workload = workload(regime, index)?;
    let cpus = cores
        .iter()
        .map(|core| {
            topology
                .iter()
                .find(|info| info.core == *core)
                .and_then(|info| info.cpus.first())
                .copied()
                .ok_or(TruthError::Identity)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let roles = match regime {
        Regime::R8 => TrialRole::neighbours(
            workload,
            cores[0],
            self::workload(Regime::R2, 0)?,
            0,
            topology,
        ),
        Regime::R9 => TrialRole::all_threads(workload, cpus[0], topology),
        Regime::R1
        | Regime::R2
        | Regime::R3
        | Regime::R4
        | Regime::R5
        | Regime::R6
        | Regime::R7 => TrialRole::legacy(regime, workload, 0, cores, &cpus, topology),
    }
    .map_err(TruthError::Roles)?;
    let spec = TrialSpec {
        id: String::new(),
        regime,
        workload,
        condition: if cores.len() == 1 {
            Condition::Alone
        } else {
            Condition::Together
        },
        cores: TrialRole::cores(&roles),
        cpus: TrialRole::cpus(&roles),
        roles,
        duration,
        index: 0,
        seed: 0,
        offsets: PerCore::default(),
        controller: togi_machine::ControllerMode::Managed {
            rotation: 0,
            purpose: togi_machine::Purpose::Exact,
        },
    };
    spec.check_roles(topology).map_err(TruthError::Roles)?;
    TrialClass::from_spec(&spec)
}
