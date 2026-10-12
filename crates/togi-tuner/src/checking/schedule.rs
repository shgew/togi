use std::collections::BTreeMap;

use togi_machine::{CcdId, CoreSet, Regime, WorkloadId, pick_workload};

use super::chains::R7Step;
use crate::config::TunerConfig;
use crate::{ClassKey, Secs, Topology};

const SHORT_TRIALS: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartRole {
    Ccd,
    Partial,
    AllCore,
}

/// The passes one trial class must have earned by this requirement; a class repeated in a step
/// or an earlier step of the same workload adds to the later requirement and leaves the earlier
/// at zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Quota {
    pub(crate) class: ClassKey,
    pub(crate) count: u32,
}

/// The trials of one loaded set within a step: a core, the whole machine, or one R7 part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PartPlan {
    pub(crate) cores: CoreSet,
    pub(crate) ccd: Option<CcdId>,
    pub(crate) quotas: Vec<Quota>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StepPlan {
    pub(crate) regime: Regime,
    pub(crate) workload: WorkloadId,
    pub(crate) parts: Vec<PartPlan>,
}

impl StepPlan {
    pub(crate) fn quotas(&self) -> impl Iterator<Item = &Quota> {
        self.parts.iter().flat_map(|part| part.quotas.iter())
    }
}

fn long_duration(config: &TunerConfig, ccds: usize, role: PartRole) -> Secs {
    let whole = config.checking_all_core.get();
    let quarter = whole / 4;
    let secs = match (ccds, role) {
        (0 | 1, _) => whole,
        (_, PartRole::Ccd | PartRole::Partial) => quarter,
        (_, PartRole::AllCore) => whole.saturating_sub(
            u32::try_from(ccds)
                .unwrap_or(u32::MAX)
                .saturating_mul(quarter),
        ),
    };
    std::num::NonZeroU32::new(secs).map_or(Secs::MIN, Secs::new)
}

fn r7_part(
    config: &TunerConfig,
    topo: &Topology,
    workload: WorkloadId,
    cores: CoreSet,
    ccd: Option<CcdId>,
    role: PartRole,
) -> PartPlan {
    let long = long_duration(config, topo.ccds().len(), role);
    PartPlan {
        cores,
        ccd,
        quotas: vec![
            Quota {
                class: ClassKey::new(workload, cores, config.short_trial),
                count: SHORT_TRIALS,
            },
            Quota {
                class: ClassKey::new(workload, cores, long),
                count: 1,
            },
        ],
    }
}

fn r7_parts(
    config: &TunerConfig,
    topo: &Topology,
    workload: WorkloadId,
    started: Option<&R7Step>,
) -> Vec<PartPlan> {
    let mut parts = Vec::new();
    for &(ccd, cores) in topo.ccds() {
        parts.push(r7_part(
            config,
            topo,
            workload,
            cores,
            Some(ccd),
            PartRole::Ccd,
        ));
        for node in started.into_iter().flat_map(|step| step.current(ccd)) {
            if node.cores.len() >= 2 {
                parts.push(r7_part(
                    config,
                    topo,
                    workload,
                    node.cores,
                    Some(ccd),
                    PartRole::Partial,
                ));
            }
        }
    }
    if topo.ccds().len() > 1 {
        parts.push(r7_part(
            config,
            topo,
            workload,
            topo.all(),
            None,
            PartRole::AllCore,
        ));
    }
    parts
}

fn single_class_part(cores: CoreSet, class: ClassKey) -> PartPlan {
    PartPlan {
        cores,
        ccd: None,
        quotas: vec![Quota { class, count: 1 }],
    }
}

fn parts_of(
    config: &TunerConfig,
    topo: &Topology,
    regime: Regime,
    workload: WorkloadId,
    started: Option<&R7Step>,
) -> Vec<PartPlan> {
    match regime {
        Regime::R1 | Regime::R2 | Regime::R3 | Regime::R4 | Regime::R5 => topo
            .order()
            .iter()
            .map(|&core| {
                let cores: CoreSet = std::iter::once(core).collect();
                single_class_part(cores, ClassKey::new(workload, cores, config.checking_trial))
            })
            .collect(),
        Regime::R6 => vec![single_class_part(
            topo.all(),
            ClassKey::new(workload, topo.all(), config.checking_idle),
        )],
        Regime::R7 => r7_parts(config, topo, workload, started),
    }
}

/// Step `index` of a cycle of `steps`, whose requirements add to those of the steps `earlier`
/// before it that run the same workload: R1 to R5 one trial per core, R6 one all-core trial, R7
/// each full, derived partial and all-core part's three short trials and one long trial.
/// Repeated classes add up.
fn step_plan(
    config: &TunerConfig,
    topo: &Topology,
    steps: &[Regime],
    r7: &BTreeMap<usize, R7Step>,
    earlier: &[StepPlan],
    index: usize,
) -> StepPlan {
    let regime = steps[index];
    let occurrence = steps[..index]
        .iter()
        .filter(|&&other| other == regime)
        .count();
    let workload = pick_workload(regime, occurrence).id();
    let mut parts = parts_of(config, topo, regime, workload, r7.get(&index));
    for part in &mut parts {
        for quota in &mut part.quotas {
            let earlier: usize = earlier
                .iter()
                .filter(|plan| plan.workload == workload)
                .map(|plan| {
                    plan.parts
                        .iter()
                        .filter(|other| other.cores == part.cores)
                        .count()
                })
                .sum();
            let earlier = u32::try_from(earlier).unwrap_or(u32::MAX);
            quota.count = quota
                .count
                .saturating_add(quota.count.saturating_mul(earlier));
        }
    }
    let mut last: BTreeMap<ClassKey, (usize, usize)> = BTreeMap::new();
    for at in 0..parts.len() {
        for slot in 0..parts[at].quotas.len() {
            let class = parts[at].quotas[slot].class;
            if let Some((p, q)) = last.insert(class, (at, slot)) {
                let carried = std::mem::take(&mut parts[p].quotas[q].count);
                parts[at].quotas[slot].count = parts[at].quotas[slot].count.saturating_add(carried);
            }
        }
    }
    StepPlan {
        regime,
        workload,
        parts,
    }
}

/// The checking requirements of every step of a cycle.
pub(crate) fn build(
    config: &TunerConfig,
    topo: &Topology,
    steps: &[Regime],
    r7: &BTreeMap<usize, R7Step>,
) -> Vec<StepPlan> {
    let mut plans: Vec<StepPlan> = Vec::with_capacity(steps.len());
    for index in 0..steps.len() {
        let plan = step_plan(config, topo, steps, r7, &plans, index);
        plans.push(plan);
    }
    plans
}

/// The steps of `plans` that change when the R7 chains of the steps `changed` do: those steps and
/// every later step of the same workload, whose requirements add to theirs. In ascending order.
pub(crate) fn dirty(plans: &[StepPlan], changed: &[usize]) -> Vec<usize> {
    let mut dirty: Vec<usize> = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        let follows = |&at: &usize| at < index && plans[at].workload == plan.workload;
        if changed.contains(&index) || dirty.iter().any(follows) {
            dirty.push(index);
        }
    }
    dirty
}

/// Builds the steps `dirty` again, in ascending order, leaving the others as they were.
pub(crate) fn rebuild(
    config: &TunerConfig,
    topo: &Topology,
    steps: &[Regime],
    r7: &BTreeMap<usize, R7Step>,
    plans: &mut [StepPlan],
    dirty: &[usize],
) {
    for &index in dirty {
        let (earlier, rest) = plans.split_at_mut(index);
        rest[0] = step_plan(config, topo, steps, r7, earlier, index);
    }
}

/// What a cycle of `steps` covers, against the coverage a full cycle needs.
pub(crate) fn coverage(steps: &[Regime]) -> (bool, Vec<String>) {
    let mut missing = Vec::new();
    for (regime, want) in togi_config::FULL_CYCLE {
        let got = steps.iter().filter(|&&step| step == regime).count();
        if got < want {
            missing.push(format!("{regime}: {} more steps", want - got));
        }
    }
    (missing.is_empty(), missing)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use togi_journal::Seq;
    use togi_machine::CcdId;

    use super::super::chains::Node;
    use super::*;
    use crate::Profile;
    use crate::testkit::{Journal, set};

    fn seq(at: u64) -> Seq {
        Seq::new(at).unwrap()
    }

    fn partial(profile: Profile) -> Node {
        Node {
            part: NonZeroU32::MIN,
            cores: set(&[0, 1]),
            profile,
            seq: seq(9),
        }
    }

    #[test]
    fn a_chain_reaches_its_step_and_the_later_steps_of_the_same_workload_and_no_others() {
        let j = Journal::has_room(&[-10, -10, -10, -10]);
        let run = j.run();
        let (config, topo) = (&run.config, run.topo_ref());
        let profile = j.profile(&[-10, -10, -10, -10]);
        let repeats = togi_machine::workloads(Regime::R7).len();
        let mut steps = vec![Regime::R1];
        steps.extend(vec![Regime::R7; repeats + 2]);
        let late = 1 + repeats;
        let mut r7: BTreeMap<usize, R7Step> = BTreeMap::new();
        let mut started = R7Step::new(seq(5));
        started.record(CcdId(0), partial(profile), Some(&profile));
        r7.insert(late, started);
        let mut plans = build(config, topo, &steps, &r7);
        assert_eq!(plans[1].workload, plans[late].workload);

        let mut early = R7Step::new(seq(4));
        early.record(CcdId(0), partial(profile), Some(&profile));
        r7.insert(1, early);
        let reached = dirty(&plans, &[1]);
        let expected: Vec<usize> = (0..steps.len())
            .filter(|&at| at == 1 || (at > 1 && plans[at].workload == plans[1].workload))
            .collect();
        assert_eq!(reached, expected);
        assert!(reached.contains(&late));
        rebuild(config, topo, &steps, &r7, &mut plans, &reached);
        assert_eq!(plans, build(config, topo, &steps, &r7));
    }
}
